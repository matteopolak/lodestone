# Redstone: dust, torches, repeaters, comparators, observers

Issues [#314](https://github.com/matteopolak/lodestone/issues/314) (parent:
dust/torch signal propagation), [#315](https://github.com/matteopolak/lodestone/issues/315)
(repeaters and comparators), [#317](https://github.com/matteopolak/lodestone/issues/317)
(observers).

## What it is

Five new modules in `crates/lodestone-server/src/`, all pure query/decision
functions with no `ChunkColumn` in scope except through a `lookup: Fn(BlockPos)
-> String` closure — the same "pure decision, fake world via closure" shape
[`docs/tick-scheduling.md`](./tick-scheduling.md) already established for
gravity blocks:

- [`redstone.rs`](../crates/lodestone-server/src/redstone.rs) — the shared
  query layer every other module composes: conductor/source predicates,
  weak/direct signal (`weak_signal`/`direct_signal`, vanilla's
  `getSignal`/`getDirectSignal`), `best_neighbor_signal`
  (`getBestNeighborSignal`), and `control_input_signal`/`alternate_signal`/
  `input_signal` (repeaters/comparators' own reads). [`make_lookup`] builds
  the `lookup` closure from a real `ChunkColumn`.
- [`redstone_wire.rs`](../crates/lodestone-server/src/redstone_wire.rs) —
  dust's own `calculateTargetStrength`/`getIncomingWireSignal`
  (`DefaultRedstoneWireEvaluator.java`/`RedstoneWireEvaluator.java`).
- [`redstone_torch.rs`](../crates/lodestone-server/src/redstone_torch.rs) —
  the 2-tick-delayed on/off inversion, standing and wall torches.
- [`redstone_diode.rs`](../crates/lodestone-server/src/redstone_diode.rs) —
  repeaters (delay, lock) and comparators (compare/subtract, cited from
  `DiodeBlock`/`RepeaterBlock`/`ComparatorBlock.java`).
- [`redstone_observer.rs`](../crates/lodestone-server/src/redstone_observer.rs)
  — the 1-tick-wide pulse out the back face.

## How it works

### The reduced conductor/source model

This crate has no collision-shape system, so a "redstone conductor" is
approximated as **anything that is not air/fluid and not a redstone
component itself** — every ordinary solid block qualifies, matching vanilla
for every block this crate's worldgen actually places. Only **lit redstone
torches** are power sources for #314 — no levers, buttons, or
`redstone_block`, since none of those exist anywhere else in this crate yet
(placement, items, or otherwise). Repeaters/comparators (when `POWERED`) and
observers (when `POWERED`) become sources too, for #315/#317.

### Direction convention

Every function that takes a `direction: Direction` parameter uses vanilla's
own convention: `direction` is the direction *travelled from the querying
position to reach the neighbour holding `state`* — i.e.
`querier.relative(direction) == neighbour_pos`. `weak_signal`'s own doc
comment states this once; every other function in `redstone.rs` shares it.
Getting this backwards (treating a diode's `FACING` as "the direction its
output travels" instead of "the direction its own `getSignal`/`getDirectSignal`
fire in") is the exact mistake seven of this landing's own first-draft tests
made — caught by the tests themselves, not by inspection. See
`redstone_diode.rs`'s own module doc for the corrected reading: `FACING` is
the direction a diode's `getInputSignal` reads *from* (its input side); the
output travels the opposite way.

### The `shouldSignal` trick, and why it has to reach `direct_signal` too

`RedStoneWireBlock.getBlockSignal` (`RedStoneWireBlock.java:285-290`) holds a
private `shouldSignal` flag false for the duration of a wire's own
`getBestNeighborSignal` call, so the wire never counts an adjacent wire's
*power* as if it were a source (that's `getIncomingWireSignal`'s job,
separately, with its own `-1` decay). Because `shouldSignal` is a field on
the **one shared `RedStoneWireBlock` instance** every wire in the world
resolves to, it suppresses **every** wire's contribution during that one
call — including a wire's `getDirectSignal` (strong power into a conductor
it touches), not just its `getSignal` (weak power). Missing the direct-signal
half let a wire sitting on a conductor relay a second wire's current power
straight through `signal_at`'s conductor-wrap, bypassing the `-1` decay
entirely — caught by a test predicting `14` and measuring `15`. `ignore_wire`
now threads through `weak_signal`, `direct_signal`, `direct_signal_to`, and
`signal_at`/`best_neighbor_signal` uniformly.

### Neighbour-update cascade (the second real reaction after gravity)

`crate::random_tick::propagate_and_react` (renamed from
`propagate_and_settle_gravity`, now that redstone is a second reaction) is
called once per just-mutated position and dispatches, per notified
neighbour:

1. Gravity settle (#311, unchanged).
2. **Dust**: recomputes target strength; if changed, writes the new `power`
   and re-fans-out through `random_tick::wire_update_fan_out`, which is
   `DefaultRedstoneWireEvaluator.updatePowerStrength`'s **complete** update
   set: seven *centres* — the wire's own position plus each of its six
   neighbours — each getting a full six-direction `updateNeighborsAt`, so 42
   notifications with duplicates among them. Vanilla issues those duplicates
   too; its `HashSet` dedupes the centres, not the notifications. Vanilla's
   iteration order over that set is unspecified and cannot be copied, so this
   picks the one deterministic order available: centres in
   `[pos] ++ UPDATE_ORDER`, six directions in `UPDATE_ORDER` within each.

   **The second layer is not a corner case, and this cost a landing.** An
   earlier version implemented centre 0 only and described the omission as "a
   diagonal-over-conductor corner update". The geometry it actually misses is
   the **standard torch-inverter** — dust on top of a block with a torch on
   that block's side. The torch is diagonal to the dust, so it is a neighbour
   of a *neighbour* and appears only in the second layer.

   **Both halves of the fix are load-bearing.** Applying the seven centres
   only to a wire reached mid-cascade is not enough: `propagate_and_react`'s
   own origin needs them too, because `NeighborPropagator::propagate` fans out
   exactly one layer from whatever position it is handed. Only dust gets this
   treatment — every other mutation family mirrors `setBlockAndUpdate`, a
   single `updateNeighborsAt(pos)`.
3. **Torches/repeaters/comparators/observers**: schedule a delayed recheck
   into `block_ticks` when steady-state disagrees with current state. No
   immediate mutation — the flip runs when the schedule drains.

### Scheduled-tick production (the second real producer, after nothing)

`tick::run_tick_loop`'s `block_ticks.drain_due(...)` loop, previously an
acknowledged island (drained every tick, nothing ever scheduled into it), now
dispatches four kinds — `redstone::TICK_TORCH`/`TICK_REPEATER`/
`TICK_COMPARATOR`/`TICK_OBSERVER` — to each family's own `run_scheduled_tick`,
and re-propagates any resulting mutation through the same `propagate_and_react`
call site a random tick uses, so a chain reaction (a repeater flipping and
feeding a further torch) resolves depth-first within one drain pass, matching
vanilla's `LevelTicks::runCollectedTicks` invoking its callback once per due
entry in `DRAIN_ORDER`.

### What reaches a client

Every mutation this landing makes — dust power changes, torch/diode/observer
flips, and every cascade they trigger — goes through the exact same
`ChunkSource::set_block` + `BlockTickFeed::publish` path random ticks and
gravity blocks already use, with zero changes to `server.rs`'s wire-forwarding
arm. A circuit built entirely from blocks a client can already see (torches
and dust; repeaters/comparators/observers are placed the same way, this
landing did not add placement) will animate correctly end to end.

**Dust propagation is now verified against a live 26.2 oracle** (issue #314,
`redstone_oracle_gate.rs`). Attenuation was measured on the real server over
RCON — power 15 in the square adjacent to a source, decaying exactly 1 per
block, reaching 0 at distance 16 — three times, from two different source
blocks, agreeing exactly each time. The gate reproduces that profile at all
15 coordinates through `propagate_and_react`, and separates it from an
off-by-one model (which differs at every coordinate) and a no-decay model
(14 of 15). Two controls were run and observed: removing the `-1` decay
fails the gate at a named coordinate, and dropping the event publication
while keeping the column write leaves every server-side assertion passing
and fails only the client-delivery one.

Repeaters, comparators and observers are **still** unverified against a live
oracle — the evidence for those remains this crate's own per-tick sequence
assertions (`redstone_diode.rs`'s pulse-quantization and compare-mode
cascade tests), not a captured trace. An order-sensitive circuit (a
T-junction, a repeater-locked latch) is the strongest remaining step.

## How to change it, and the gotchas

- **Player placement is now a third `propagate_and_react` caller, and it is the
  only one a player can trigger** (issue #465). `server.rs`'s
  `propagate_placement` runs the same fan-out `tick.rs` already runs after a
  random tick and after a drained scheduled tick, then persists every returned
  change through `ChunkSource::set_block` and sends a `BLOCK_UPDATE` for each.
  Before it existed the subsystem was reachable only by a random tick landing
  next to a circuit, and dust and torches are not randomly-ticking blocks — so
  in practice, not at all. Gated by
  `crates/protocol/v770/tests/server_redstone_placement.rs`.
- **Placement resolves only the synchronous half.** `propagate_placement`'s
  `ScheduledTickQueue` is local and discarded, so dust (synchronous in vanilla,
  measured at 0 ticks) completes, while a torch/repeater/comparator/observer —
  which react by *scheduling* a recheck the tick loop owns — still does nothing
  when placed next to a live circuit. Closing that needs `tick.rs` to drain a
  queue fed from the connection; see #465's own thread for the patch.
- **A partial block-state string does not survive the v770 encoder.**
  `resolve_state_id` (`crates/protocol/v770/src/server_protocol.rs`) matches by
  **exact property set**. `minecraft:redstone_wire` has 1296 states carrying
  five properties, while `redstone_wire::set_power` deliberately emits only
  `minecraft:redstone_wire[power=N]` — so no dust state ever matches, and the
  encoder falls through to the lowest id with that name, `4011`, which is
  `power=0`. **Every dust change this server sends is delivered to the client
  as zero**, whatever it computed, and that has always been true — of random
  ticks and scheduled ticks as much as of placement. It is a fully-connected
  wire carrying the wrong value, which `cargo xtask connectedness` structurally
  cannot see. Measured by
  `server_redstone_placement::the_powered_run_reaches_the_client`, which is
  `#[ignore]`d and names this as its blocker; the fix is a subset match in
  `resolve_state_id`, not a change to what this crate emits.
- **Never reason about `FACING` as "output direction."** It is the direction
  a diode/observer's own `getSignal`/`getDirectSignal`/`getInputSignal` read
  *from* — see "Direction convention" above. Every wrong test in this landing
  made this exact mistake.
- **A bare torch can never be a side/alternate input.** `getControlInputSignal`'s
  `!only_diodes` branch reaches `getDirectSignal`, and a torch's own
  `getDirectSignal` is nonzero only for `direction == Down` — a torch beside
  a comparator cannot feed its side input at all; only a wire (read directly
  via `POWER`, bypassing direction entirely) or another diode's own output
  can.
- **The comparator's analog output lives in a block-state property
  (`output=N`), not a block entity.** `crate::block_entities` exists in this
  crate but nothing threads a `BlockEntityRegistry` through this module's
  call chain — see `redstone_diode.rs`'s own doc comment for why this is a
  real, bounded substitution (same meaning, different storage), not an
  invented shortcut.
- **Container-reading comparators are a named, uncloseable-today gap.**
  `ComparatorBlock.getInputSignal`'s analog-output/item-frame branch (issue
  #315's own "hopper fill level" trap) is not implemented — every comparator
  test in this landing reads only redstone-native inputs.
- **Adding a new power source** (a lever, a button, `redstone_block`): extend
  `redstone::own_signal`/`weak_signal`/`direct_signal`/`is_signal_source`
  with the new predicate, following the same per-block-class dispatch every
  existing family uses.
- **The second-layer fan-out has landed — do not re-narrow it.** See "How it
  works" above for the shape. Gated by
  `redstone_oracle_gate::the_second_layer_fan_out_reaches_a_side_torch_and_inverts_it`,
  with `..._is_left_alone_when_the_source_is_unlit` as its paired control.
  Reverting `wire_update_centres` to `vec![pos]` was run as a control and both
  that gate **and** the comparator end-to-end gate fail, the former naming the
  coordinate: *"the torch at (x=6, y=1, z=8) was never notified … the dust on
  its attachment block at (x=5, y=2, z=8) is at power 13"*.
- **A rig that seeds dust at its settled power is vacuous once the second
  layer is live.** The pinning test written while the gap was open set the
  dust to 15 by hand with nothing feeding it. That was harmless then (the
  first layer never re-evaluated the dust) and is fatal now, in the
  *safe-looking* direction: the cascade reaches the dust, it correctly
  recomputes to 0 for want of a source, the attachment block stops being
  powered, and the torch is then correctly **not** scheduled — a passing test
  proving nothing. A premise check does not catch this, because the premise is
  true when checked and false by the time it matters. **Give the rig a real
  source, start the dust at zero, and check the premise *after* propagation.**
- **Nothing triggers redstone from player action.** The only callers of
  `propagate_and_react` are a random tick that mutated a block and the
  `block_ticks` drain. `server.rs`'s block-placement path (`apply_use_item_on`)
  neither calls it nor schedules anything — and it writes `STONE` rather than
  the held item, so dust cannot be placed by a player at all. Until both are
  addressed, redstone is reachable only from a random tick that happens to
  mutate a block adjacent to a circuit.

### Oracle traps, if you build a live redstone gate

Every one of these cost real time, and they all fail in the *safe-looking*
direction — the rig reports a plausible "nothing happened" rather than an
error:

- **`/setblock` does not reproduce a power source's natural update fan-out.**
  `LeverBlock.updateNeighbours` — which notifies the attached block *and that
  block's own neighbours* — runs from the lever's use/removal handlers, not
  from `setBlock`. A lever flipped with `/setblock` powers its block but never
  notifies a torch two blocks away, which sits there lit forever. Use redstone
  **dust** as the trigger: its evaluator does that fan-out on every power
  change, through the ordinary neighbour-update path.
- **`pause-when-empty-seconds` defaults to `60`, and this is the one that will
  waste your afternoon.** With no player connected the dedicated server pauses
  the whole world after a minute. `gameTime` stops dead, and since
  `ServerLevel.tick` runs `this.blockTicks.tick(this.getGameTime(), ...)`,
  **no scheduled block tick ever fires again**. Redstone then looks simply
  dead: dust still propagates (it is synchronous, inside the `setBlock`
  itself) while every repeater, comparator, observer and torch sits inert
  forever, and `/tick step` appears to do nothing. Set
  `pause-when-empty-seconds=0` in the oracle world's `server.properties` and
  restart. **Control first, every session**: `/setblock` a `minecraft:sand`
  block in the air; if it does not land, nothing is ticking and every timing
  reading you are about to take is vacuous.
- **`/tick step N` *does* advance scheduled block ticks.** An earlier revision
  of this document said it did not. `TickRateManager.tick` sets
  `runGameElements = !isFrozen || frozenTicksToRun > 0`, and `ServerLevel.tick`
  gates `blockTicks.tick(...)` on exactly that (`ServerLevel.java:358,386-389`)
  — a stepped tick runs them normally. The original observation was the
  paused-world symptom above. `/tick freeze` plus one `/tick step 1` at a
  time, confirming each landed by reading `time query gametime`, is the way to
  take a tick-exact redstone measurement.
- **`/tick sprint N` returns immediately and a following `/tick unfreeze`
  interrupts it**, so a sprint used to settle a rig may run almost no ticks.
- **Force-loading is enough; a player is not needed.** `TicketType.FORCED`
  carries `FLAG_SIMULATION`, so `/forceload add` gives a ticking region — once
  the pause above is disabled.
- **A `powered=true` comparator does not lock a repeater.** A diode's lock
  contribution is its *output signal*, and a comparator's is its stored analog
  output, not its `powered` flag. A freshly `/setblock`-ed
  `comparator[powered=true]` has output 0 and locks nothing — measured, and it
  looks like a modelling bug when it is not.

### What the live 26.2 oracle measured (#315/#317)

Tables live in `redstone_diode_oracle_gate.rs`; the summary:

| quantity | measured |
|---|---|
| repeater delay, property `d` | **`2d` game ticks** (2/4/6/8), identical on the rising and falling edge |
| repeater orientation | `facing` names the **input** side; output leaves the opposite face |
| repeater lock | only a **powered diode** whose own `facing` matches the queried side; a lit torch, a `redstone_block`, an unpowered repeater and a wrong-way repeater all fail to lock |
| comparator delay | **2 game ticks**, both edges, both modes |
| comparator output | 30 rows across both modes, all reproduced by `calculate_comparator_output` |
| observer pulse | back face high on ticks **2 and 3**, i.e. starts at 2 and is **2 game ticks** wide |
| observer trigger | block placed, block removed **and** a pure block-state change all pulse identically; a change behind it does not, and a no-op `setblock` does not |

## Configuration

No new constants beyond each family's own vanilla-cited literals: torch
delay `2` ticks, repeater delay `DELAY * 2` (`2, 4, 6, 8`), comparator delay
`2` ticks, observer pulse `2` ticks (on then off).

## Dependencies

- `crate::neighbor_update::{Direction, NeighborPropagator, ALL_DIRECTIONS}` —
  the six-direction primitives every query and cascade in this family
  builds on; `Direction::opposite`/`clockwise`/`counterclockwise` were added
  to that module for this landing.
- `crate::scheduled_tick::{ScheduledTickQueue, TickPriority}` — the delayed
  recheck queue, gaining its first real production caller here.
- `crate::chunk::{ChunkColumn, is_air_or_fluid}` — the world representation
  every `lookup` closure reads through.

[`make_lookup`]: ../crates/lodestone-server/src/redstone.rs
