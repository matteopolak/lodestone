# Redstone: dust, torches, repeaters, comparators, observers, and the input devices

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
- [`redstone_openable.rs`](../crates/lodestone-server/src/redstone_openable.rs)
  — doors, trapdoors and fence gates opening when powered and closing when
  unpowered (issue #319). The *consumer* families: unlike the four above they
  emit no signal, so they live entirely outside `redstone.rs`'s
  source/conductor model.

## How it works

### The reduced conductor/source model

This crate has no collision-shape system, so a "redstone conductor" is
approximated as **anything that is not air/fluid and not a redstone
component itself** — every ordinary solid block qualifies, matching vanilla
for every block this crate's worldgen actually places. `is_redstone_component`
is that exclusion list, and it is specifically the set of modelled blocks that
are **not full cubes**: `minecraft:target` and `minecraft:redstone_block` are
signal sources that stay on the *conductor* side of it, because both register a
full collision cube in the jar. Getting `redstone_block` onto the wrong side
would stop it powering a wire across a conductor at all.

**Relaying** sources: lit redstone torches, repeaters/comparators when
`POWERED`, observers when `POWERED`.

**Input** sources — the primary devices a player reaches for — are
`redstone::is_input_source`'s nine, and they do **not** all emit 15:

| family | signal while active | strong power reaches |
|---|---|---|
| lever, button | 15 | the surface it is attached to (`getConnectedDirection`) |
| pressure plate | 15 | the block below it |
| weighted pressure plate | its own `power`, `0..=15` | the block below it |
| tripwire hook | 15 | the wall it faces |
| detector rail | 15 | the block below it |
| target | its own `power`, `0..=15` | nothing |
| daylight detector | its own `power`, `0..=15` | nothing |
| redstone block | 15 | nothing, but see below |

Two facts about that table are load-bearing and neither is guessable:

* **None of the nine overrides `getSignal`.** They stop at `ownSignal`, so each
  emits weakly in **all six directions** — unlike every relaying family, each of
  which excludes at least one. A lever really does weakly power a wire directly
  above it. Copying the torch's "every direction except UP" shape is the wrong
  guess here.
* **`target`, `daylight_detector` and `redstone_block` send no strong power at
  all**, because none overrides `getDirectSignal` either. A block of redstone
  reaches a comparator's side input only through
  `SignalGetter.getControlInputSignal`'s explicit `is(Blocks.REDSTONE_BLOCK)`
  branch, which sits *before* the wire check. Without that one arm a block of
  redstone supplies no side input and the comparator looks broken.

**The read is wired; several producers are not.** Something has to write the
`powered`/`power` property, and only some families have that half. `hand_use`
flips a lever and a button from a right-click and `server`'s use-item-on path
fans it out, so those two work end to end; `redstone_block` needs nothing.
Pressure plates, weighted plates and detector rails need an entity-AABB census
this crate has no collision system for; a tripwire hook needs `minecraft:tripwire`
state plus the two-hook span search; a target needs projectile-hit dispatch and
its decay tick; a daylight detector needs a sky-light read. Those five sit at
their default `0` — correct reads with missing producers, listed in
`redstone.rs`'s own module doc.

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
4. **Redstone-openable blocks (#319)**: doors/trapdoors/fence gates read
   `best_neighbor_signal > 0` and, when it differs from their stored
   `powered`, write both `open` and `powered` to it **immediately** — the
   hopper arm's shape (vanilla's flag-2 `neighborChanged`, no `scheduleTick`),
   not the delayed-recheck families'. A two-high door flips **both** halves in
   one go: this crate has no `updateShape` pass for vanilla's half-sync to
   live in, so the other half is written by the same arm and no cascade is
   returned (matching flag 2's no-fan-out).

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
- **Adding a new power source**: extend
  `redstone::own_signal`/`weak_signal`/`direct_signal`/`is_signal_source`
  with the new predicate, following the same per-block-class dispatch every
  existing family uses — and **read the block class's own `getSignal` and
  `getDirectSignal` rather than copying a neighbouring family's**. The two are
  independent: `weak_signal` and `direct_signal` were separately neutered as
  controls, and removing only the lever's `direct_signal` arm left **nine of ten**
  redstone gates green, failing solely
  `redstone::tests::a_lever_on_the_side_of_a_conductor_powers_a_wire_on_top_of_it`.
  A weak-only implementation therefore looks almost entirely correct.
- **A new source silently invalidates every gate whose premise was "this family
  is inert".** `redstone_oracle_gate`'s dust runs all used a torch, which was the
  one source the old model got right, so the whole file shared a blind spot;
  `piston.rs` carried a doc comment stating levers emit no signal. Grep for the
  family name before assuming a green suite means anything.
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
- **The openable families are the one redstone family with *no* producer of
  any kind yet** (#319): doors/trapdoors/fence gates are absent from
  `placed_block_state` and from worldgen, so the reaction code in
  `redstone_openable.rs` is exercised only by tests until placement or
  worldgen lands one. It is wired into the same `react_to_notification` arm
  every other family uses, so the moment a door exists in a column next to a
  circuit it starts working — the wiring is not the gap, the producer is.

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

## Pistons are a separate module

`crate::piston` (issue #316) reaches its signal test through this module's `signal_at`, and its
reaction arm sits beside the repeater and comparator arms in `random_tick.rs`. Two things worth
knowing from here:

* **`has_extend_signal` is not `best_neighbor_signal`.** A piston's `getNeighborSignal` excludes the
  push direction and additionally reads `pos.above()` — quasi-connectivity. Reaching for the general
  best-neighbour query there is the mistake.
* **`minecraft:redstone_block` is a signal source now**, along with the other eight input devices;
  `piston::tests::the_placement_path_carries_the_commits_when_a_lever_is_the_trigger` is the gate
  that a lever drives a piston, which it could not do before.

See [`redstone-pistons.md`](./redstone-pistons.md) for what is and is not modelled.

## Devices: rails, dispensers/droppers, note blocks, tripwire, target (#318/#320/#322)

Five more modules, same "pure decision, `lookup` closure" shape as the family
above: [`redstone_rail.rs`](../crates/lodestone-server/src/redstone_rail.rs),
[`redstone_dispenser.rs`](../crates/lodestone-server/src/redstone_dispenser.rs),
[`redstone_note_block.rs`](../crates/lodestone-server/src/redstone_note_block.rs),
[`redstone_tripwire.rs`](../crates/lodestone-server/src/redstone_tripwire.rs),
[`redstone_target.rs`](../crates/lodestone-server/src/redstone_target.rs).

**Triage first, because two of the three issues' "what exists: nothing" was
already stale.** A prior landing (the lever/button/plate/tripwire-hook/
detector-rail/target/daylight-detector/redstone-block signal-*read* pass)
had already put tripwire hook's and target's `ownSignal`/`getDirectSignal`
arms and `is_signal_source` membership into `redstone.rs`, and detector
rail's alongside them — see that module's own "what sources exist today"
table. This landing is the *producer* half for those three, plus rails and
dispensers/droppers from a standing start.

| device | what landed | what did not, and why |
|---|---|---|
| powered/activator rail | `POWERED` tracking: direct signal or an up-to-8-cell chain of same-shape powered rails, both classes confirmed to be one Java class registered twice | `SHAPE`/curve connectivity (`BaseRailBlock`'s own placement algorithm) — a block-placement concern, not redstone |
| detector rail | (read already landed) | producer: needs a real `AbstractMinecart`, which this crate has none of (#11). **The issue body's own suggested test — a dropped item — is stale**: vanilla's `getInteractingMinecartOfType` filters to `AbstractMinecart` specifically, so a dropped item never triggers it in real 26.2 either |
| dispenser/dropper | the shared `TRIGGERED` state machine; the dropper/dispenser boundary (`getDispenseMethod`) documented and the ~35-entry behaviour table enumerated from the jar's own `bootStrap`; **the fire itself** (issue #320) — `tick.rs`'s `TICK_DISPENSER_FIRE` arm reads the live 9-slot `generic_3x3` container through the `BlockEntityHandle`, picks a slot with `random_slot`, and dispatches per item: a dropper always either pushes one item into a container ahead (`crate::hopper::try_move_item_into`, gated on `is_pushable_container`) or plain-tosses, never consulting the item table; a dispenser matches a spawn egg (`MobSim::spawn_species`), a boat/chest-boat/raft (`boat_dispense`, `MobSim::spawn_vehicle`), bone meal (`crate::bone_meal::apply_bone_meal`) and the fire-placement half of flint-and-steel (`flint_and_steel_ignite`) in turn, falling to `plain_toss` through `MobSim::spawn_item` when none match or a matched behaviour reports no effect | most of the ~35 special item behaviours remain unmodelled, each for its own named reason in `redstone_dispenser.rs`'s own table — projectiles (arrow/egg/snowball/potions/firework/fire-charge/wind-charge, ~12 items) have a shooter-side spawn path (`MobSim::spawn_projectile_from`) but no per-item power/uncertainty config table and no potion-contents item component; TNT needs a primed-TNT entity this crate has none of at all; minecarts need a minecart entity/vehicle physics this crate has none of at all (`crate::redstone_rail` already says so); shears need a shearing/wool-state mechanism that exists nowhere in this crate, not even for a player's own direct use; every armor/equippable item needs an entity spatial query and a mob equipment-slot model neither of which exist; buckets need an item-triggered fluid place/pickup entry point, which does not exist (`crate::fluid` only ticks a fluid already in the world); wither skull and carved pumpkin need `BlockPattern` multi-block shape matching (wither/golem shapes); an empty container is a silent no-op rather than vanilla's click sound, since this crate does not model sound effects here yet |
| note block | instrument selection (`setInstrument`, partial per-block table — 9 single-block overrides + 7 heads + the small `SNARE` family, `BASS`/`BASEDRUM`'s ~330 blocks unmodelled and documented as such); the `POWERED` pulse reaction, rising-edge-only, gated on audibility | right-click note cycling (needs a `hand_use.rs` hook this module does not own); the sound/particle pulse itself (`level.blockEvent` — no client-visible-effect-with-no-state-write wire path exists in this crate yet) |
| tripwire hook + tripwire | `calculateState`'s full scan/attach/power algorithm, both hooks' writes, the wire-segment `attached` fan-out, the 10-tick recheck; wired for **placement** of either block (`react_at_placement`, since vanilla drives this from `setPlacedBy`/`onPlace`, never `neighborChanged`) | **entity crossing** (`checkPressed`'s entity-AABB read — no collision census, the same gap pressure plates/detector rail already have); the **instant break-pulse** (`affectNeighborsAfterRemoval` — needs a block-*removal* callback carrying the destroyed state, which nothing in this crate's block-breaking path offers yet — `on_wire_removed` is written and ready) |
| target | `getRedstoneStrength` (hit distance -> `1..=15`), the arrow-vs-other duration split, the decay-to-zero scheduled tick, **and now the trigger** (#322): `crate::mobs::MobSim::resolve_projectile_impacts` resolves the exact struck face/fraction (`mobs/projectiles.rs`'s `block_entry`, the same per-axis slab test `clip_aabb` uses for an entity hitbox) and hands it to `crate::tick::run_tick_loop_with_weather`, which reads the live block, calls `apply_hit`, writes the new `power` and schedules the decay | none — `apply_hit`'s own seam is now a real producer end to end |

### What each device needs of the execution model (for issue #548)

The owner's #548 rework (an incrementally-invalidated dependency graph
replacing the per-tick rescan) is deliberately sequenced *after* the device
set, so this table is meant to double as its spec-in-progress rather than
prose describing the graph:

| device | trigger | propagation | scheduled tick | cross-device read |
|---|---|---|---|---|
| powered/activator rail | neighbour notification | `pos.below()` always, `pos.above()` too iff the rail's own `SHAPE` is a slope — **not** a plain six-direction fan-out | none | yes, and unusually far: up to 8 cells outward in two directions, through *other rails' own* `POWERED` |
| dispenser/dropper | neighbour notification (`pos` and `pos.above()`) | none beyond the ordinary fan-out | yes, one-shot, rising-edge only, fixed 4 ticks, never reschedules | no |
| note block | neighbour notification | none — a note block emits no signal | none | no |
| tripwire hook/tripwire | **placement**, not a neighbour notification (`setPlacedBy`/`onPlace`); periodically, a 10-tick self-recheck | up to two hook positions plus every wire cell between them — the widest blast radius here after a piston's multi-cell move | yes, conditionally (only when the scan was itself driven by one specific wire cell) | yes: the hook's own scan reads up to 41 cells outward, live, on every recheck |
| target | a projectile hit, resolved by `MobSim::resolve_projectile_impacts` and applied by `crate::tick::run_tick_loop_with_weather` (#322) | none beyond the ordinary fan-out | yes, one-shot, 20 or 8 ticks depending on projectile kind, suppressed while already pending | no |

Two things worth carrying into that rework specifically:

- **Not every device fits the `react_to_notification`/`Option<String>` single-
  write shape the diode/torch/observer family established.** Tripwire's
  multi-position write plan (`CalculatedState`) and a piston's multi-cell move
  are the same shape for the same reason — either can rewrite positions well
  outside the notified cell — and both needed their own `tick.rs`/
  `random_tick.rs` entry points (`run_tripwire_recheck`, gravity's
  `settle_gravity_at`) rather than sharing the ordinary dispatch chain. A
  graph rework needs an edge type for "one recomputation writes N nodes," not
  just "one recomputation writes its own node."
- **A device's trigger is not always a neighbour notification, and rails prove
  it two different ways in one family.** A rail reacts to `neighborChanged`
  like every other family here, *and* also has to notify itself once on
  placement (`BaseRailBlock.onPlace` calls `neighborChanged` on itself) — the
  same "the placed block owes itself a reaction the neighbour pass cannot
  deliver" shape the hopper `ENABLED` write and fire's first tick already
  established, now with a third and fourth caller (rail, tripwire hook,
  tripwire). Tripwire goes further: it has **no** `neighborChanged` at all,
  only placement and a self-scheduled poll. A graph keyed purely on "which
  positions does a `setBlock` notify" cannot express either case.

### Traps specific to this batch, and what they cost

- **A conjunction with an entity half is not "half done", it is a different
  scope.** Detector rail's read (a prior landing) and this landing's tripwire
  connectivity both look complete in isolation and both stop exactly at the
  entity-presence clause — `AbstractMinecart` for the rail,
  `!entity.isIgnoringBlockTriggers()` for the wire. Neither clause is
  guessable from the other family's shape (a rail wants *only* minecarts; a
  tripwire wants *any* non-ignoring entity), so "detector rail's producer" and
  "tripwire's entity trigger" are not one future task, they are two, gated on
  different prerequisites (#11's minecarts vs. a general collision/AABB
  census).
- **Dispensers dispatch on the *item*, not the block, and the table is wide on
  purpose.** `DispenseItemBehavior.bootStrap` registers entries across
  ~13 shapes (plain toss, projectile, boat, bucket-fill, bucket-empty,
  flint-and-steel, bone meal, TNT, wither-skull, carved-pumpkin, shulker-box,
  glass-bottle, glowstone, shears, brush, honeycomb, potion, minecart), plus
  three implicit defaults (spawn egg, equippable, sulfur-cube) `getDefaultDispenseMethod`
  falls back to when no explicit registration matches — see
  `redstone_dispenser.rs`'s own module doc table for the full enumeration
  with a jar citation per row. Treating "ejects an item" as done without
  reading that table is exactly the trap the issue body named.
- **The fire arm was a confirmed island until #320's first half landed it, and
  the special-behaviour table was a second, narrower island inside the same
  arm.** `random_slot` and `dispense_position` were implemented, individually
  tested and marked `#[allow(dead_code)]` — `tick.rs`'s scheduled-tick drain
  had no arm for `TICK_DISPENSER_FIRE` at all, so a filled, powered dispenser
  never ejected anything. That landing closed the plain-toss path; #320's
  remainder closed the dropper's container push and five of the special
  per-item behaviours (spawn egg, boat, bone meal, flint-and-steel's
  fire-placement arm) the same way — implemented and tested, but the
  `TICK_DISPENSER_FIRE` arm took only the plain-toss row regardless of item
  until this landing's dispatch chain was wired in. `is_dropper` lost its
  `#[allow(dead_code)]` with this landing (it now picks the dropper's
  container-push-or-toss path); `facing_name` remains dead — nothing added
  here needed a `Direction -> &str` conversion outside `direction_to_str`'s
  existing round trip.
- **`PoweredRailBlock` is one Java class serving two item ids.** Confirmed
  against `Blocks.java`'s own `register` calls rather than assumed — both
  `minecraft:powered_rail` and `minecraft:activator_rail` share every byte of
  the `POWERED`-tracking logic in this landing; only activator rail's
  minecart-launching side (unmodelled, needs a minecart) differs at all.
- **A round-number instinct would have under-cited the target formula.** The
  `max(1, …)` floor is easy to drop by assuming a grazing hit reads `0`; the
  quarter-offset case (`distance = 0.25` -> `ceil(7.5) = 8`) is the one this
  landing's tests pin exactly rather than merely asserting "less than 15".

## Configuration

No new constants beyond each family's own vanilla-cited literals: torch
delay `2` ticks, repeater delay `DELAY * 2` (`2, 4, 6, 8`), comparator delay
`2` ticks, observer pulse `2` ticks (on then off). This batch adds: rail
search cap `8` cells, tripwire recheck `10` ticks (max span `41` cells),
dispenser fire delay `4` ticks, target decay `20`/`8` ticks
(arrow/other).

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
