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
   and cascades the same six-direction fan-out (`UPDATE_ORDER`) from the
   wire's own position — mirroring `RedStoneWireBlock.updatePowerStrength`'s
   `level.updateNeighborsAt` calls. **Named deviation**: vanilla additionally
   fans out from each of those six neighbours' own positions too (a second
   layer, collected through a `HashSet` whose iteration order vanilla itself
   does not guarantee) — this landing implements the first layer only, which
   is the deterministic part. A straight or right-angled run of dust updates
   correctly; a diagonal-over-conductor corner update may lag by one extra
   notification round versus vanilla.
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
- **The first-layer-only fan-out is not a corner case.** `propagate_and_react`
  implements only the first layer of `DefaultRedstoneWireEvaluator`'s update
  fan-out (the wire's own position); vanilla also fans out from each of the
  six neighbours' own positions. The geometry that omission misses is the
  **standard torch-inverter** — dust on top of a block with a torch on that
  block's side, where the torch is diagonal to the dust and only the second
  layer ever reaches it. Measured live: the real server inverts that torch
  reliably; we never notify it. Pinned by
  `redstone_oracle_gate::the_second_layer_fan_out_gap_leaves_a_side_torch_unnotified`,
  which asserts today's behaviour and fails loudly when the second layer lands.
- **Nothing triggers redstone from player action.** The only callers of
  `propagate_and_react` are a random tick that mutated a block and the
  `block_ticks` drain. `server.rs`'s block-placement path (`apply_use_item_on`)
  neither calls it nor schedules anything — and it writes `STONE` rather than
  the held item, so dust cannot be placed by a player at all. Until both are
  addressed, redstone is reachable only from a random tick that happens to
  mutate a block adjacent to a circuit.

### Oracle traps, if you build a live redstone gate

Both of these cost real time and both fail in the *safe-looking* direction —
the rig reports a plausible "nothing happened" rather than an error:

- **`/setblock` does not reproduce a power source's natural update fan-out.**
  `LeverBlock.updateNeighbours` — which notifies the attached block *and that
  block's own neighbours* — runs from the lever's use/removal handlers, not
  from `setBlock`. A lever flipped with `/setblock` powers its block but never
  notifies a torch two blocks away, which sits there lit forever. Use redstone
  **dust** as the trigger: its evaluator does that fan-out on every power
  change, through the ordinary neighbour-update path.
- **`/tick step N` does not advance scheduled *block* ticks** — the known
  `tick step`/`tick sprint` trap extends past entity physics. Measured against
  a rig *proven* to work: two seconds of real time inverted the torch every
  time, while eight consecutive `/tick step 1` calls on the identical rig
  never did. Settle with real time, and take delay constants from the jar.
  Also note `/tick sprint N` returns immediately and a following `/tick
  unfreeze` **interrupts it**, so a sprint used to settle a rig may run almost
  no ticks at all.

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
