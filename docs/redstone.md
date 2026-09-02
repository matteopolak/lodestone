# Redstone: dust, torches, repeaters, comparators, pistons, devices and the beacon

## What it is

The server's redstone model: dust/torch signal propagation, repeaters,
comparators, observers, pistons, the player-facing input devices (levers,
buttons, plates, rails, dispensers, note blocks, tripwire, target blocks), and
two consumers built on top — the beacon and the vibration/warden substrate.
All of it is pure query/decision functions over a `Fn(BlockPos) -> String`
world lookup, the same shape [`docs/tick-scheduling.md`](./tick-scheduling.md)
established for gravity blocks — no `ChunkColumn` in scope except through that
closure.

## How it works

### The signal model

This crate has no collision-shape system, so a redstone **conductor** is
approximated as "anything solid that is not a redstone component itself" —
correct for every block this crate's worldgen actually places, and notably
**not** the same set as "a full cube": `minecraft:target` and
`minecraft:redstone_block` are signal sources that still count as conductors,
because both register a full collision cube in the jar.

Every function taking a `direction` parameter uses vanilla's own convention:
*the direction travelled from the querying position to reach the neighbour
holding `state`*. Reading a diode's `FACING` as "the direction its output
travels" instead of "the direction its own `getSignal` reads *from*" is the
single most repeated mistake in this whole subsystem — `FACING` is the input
side; output leaves the opposite face.

**Input sources** (lever, button, pressure plate, weighted plate, tripwire
hook, detector rail, target, daylight detector, redstone block) do **not** all
emit the same way. None of the nine overrides `getSignal`, so each emits
weakly in all six directions — unlike every relaying family (torch, diode,
observer), each of which excludes at least one direction. And three of them —
`target`, `daylight_detector`, `redstone_block` — send **no strong power at
all**: a block of redstone reaches a comparator's side input only through one
explicit `is(REDSTONE_BLOCK)` branch that sits *before* the ordinary wire
check, not through `getDirectSignal`. Missing that one arm makes a redstone
block look like it cannot feed a comparator's side input.

Several of the nine sources are wired for reads but have **no producer** yet
— nothing writes their `powered`/`power` property, because that needs an
entity-AABB census (pressure plates, weighted plates, detector rails), a
two-hook span search (tripwire — since landed, see Devices below), projectile
dispatch (target — since landed), or a sky-light read (daylight detector).
Those sit at their default of `0`: a correct read with a missing producer, not
a wrong read.

### Dust, torches, diodes, observers

Dust's own `shouldSignal` trick — a wire never counts an *adjacent* wire's
current power as a source of its own `getBestNeighborSignal` — has to reach
**both** `getSignal` and `getDirectSignal`, because vanilla implements it as
one shared flag on the one `RedStoneWireBlock` instance every wire resolves
to. Missing the direct-signal half lets a wire sitting on a conductor relay a
second wire's power straight through, bypassing the usual `-1` decay per
block.

The neighbour-update cascade dust triggers is **two layers deep**, not one: a
wire's own power change fans out through **seven centres** — its own position
plus each of its six neighbours — each getting a full six-direction
notification (vanilla's real `updatePowerStrength`, duplicates included; a
`HashSet` on the vanilla side dedupes the centres, not the notifications).
Implementing only the first layer misses the ordinary torch-inverter geometry
(dust on top of a block, a torch on that block's *side* — diagonal to the
dust, so it is a neighbour of a neighbour). Torches/repeaters/comparators/
observers instead **schedule** a delayed recheck when steady-state disagrees
with current state, rather than mutating immediately; scheduled ticks drain
through the same tick loop a random tick uses, so a chain reaction resolves
depth-first within one drain pass.

Delay constants, all live-oracle-measured: torch/comparator/observer delay 2
game ticks; repeater delay `2 × d` (2/4/6/8 for `d = 1..4`), identical on both
edges; observer pulse 2 ticks wide, starting on tick 2. A `powered=true`
comparator does **not** lock a repeater by itself — a diode's lock
contribution is its *output signal* (for a comparator, its stored analog
output), not its `powered` flag, so a freshly-placed `comparator[powered=true]`
has output 0 and locks nothing.

### A partial state string does not survive the encoder — every dust change is delivered as zero

`resolve_state_id` matches by **exact property set**, and `redstone_wire`'s
state-writer deliberately emits only `[power=N]` (one of the wire's five
properties) — so no dust state ever matches, and the encoder falls back to
the lowest id sharing the name, `power=0`. **Every dust change this server
sends has always been delivered to the client as zero**, whatever it
actually computed — a fully-connected wire carrying the wrong value, which no
connectivity scan can see. The fix belongs in the encoder (a subset match),
not in what this crate computes.

### Player placement triggers redstone; nothing else does yet

The only callers of the propagation entry point are a random tick that
mutated a block and the scheduled-tick drain — **not** block placement, until
a third caller was added specifically for it: `propagate_placement` runs the
identical fan-out and persists every returned change with a real
`BLOCK_UPDATE`. It resolves only the **synchronous** half, though — dust
completes (vanilla measures it at 0 ticks), while a torch/repeater/comparator
/observer, which react by *scheduling* a recheck the tick loop owns, still
does nothing when placed next to a live circuit, since the placement path's
own scheduled-tick queue is local and discarded.

### Pistons

`crate::piston` ports `PistonBaseBlock` and `PistonStructureResolver` as pure
decisions. Four pieces: push reactions (per-block `DESTROY`/`BLOCK`/
`PUSH_ONLY`/`NORMAL`, plus four hard-coded unpushable exceptions and
`!hasBlockEntity()`); the structure resolver itself (a 12-block search, the
order-sensitive sticky-block reordering at a collision — slime and honey do
**not** stick to each other, though each sticks to everything else); a
quasi-connectivity signal read (`has_extend_signal` is emphatically not
`best_neighbor_signal` — it excludes the push direction at the neighbour cell
but *also* reads the cell **above** the piston in every direction except
down, a real vanilla quirk every BUD switch and observer clock depends on);
and a two-phase animated move (`begin_move` splits the one-step write into
"empties now" vs. "holds a `moving_piston` placeholder for 2 ticks," and
`finish_move` **replays** the one-step writes rather than recomputing them,
so the committed world is always byte-identical to what a one-step push
would have produced).

Timing: push on tick `N`, commit on tick `N + 2` (vanilla's own block-event
vs. block-entity tick ordering) — four cells animate on a three-block push,
not three, because the piston's own arm cell is a travelling block too, and
it carries the head. The moving-block record travels as a scheduled tick's
own encoded **kind string** (there is no block-entity map on the reaction
surface), so match that kind by prefix, never equality.

Reaching a client needs two packets per animating cell in order —
`block_update` writing `minecraft:moving_piston[...]` (which makes the client
create the record), then `block_entity_data` with the moving state's actual
save tag. Either alone draws nothing. Three silent traps in that record:
`facing` is written as a **byte** (`Direction.LEGACY_ID_CODEC`, declaration
order, not alphabetical — an `Int` decodes as absent and everything animates
toward `DOWN`); `progress` must be the value **before** this tick's `+0.5`
ramp (`progressO`), since the client owns the ramp itself; and the block's
registry key (`moving_piston`) is not its block-entity's key (`piston`).

A move is interruptible: retracting checks the piston's own **arm cell** for
a still-pending commit from that same piston's own extend and forces it to
finish immediately before computing the retraction — and a second, narrower
check covers the cell a *sticky* retraction would pull from, which can belong
to a **different** piston's still-animating extension. A push cannot cross a
chunk border, because the shared world-lookup closure reads air outside its
own 16×16 footprint — a property of the whole redstone family's lookup, not
of the piston resolver itself.

### Devices: rails, dispensers/droppers, note blocks, tripwire, target

Five more pure-decision modules, same shape as the family above. Rails:
`POWERED` tracking for both powered and activator rail (confirmed to be one
Java class registered twice — only activator rail's minecart-launching side
is unmodelled, since this crate has no minecarts); curve/shape connectivity
is a placement concern, not modelled here. Dispensers/droppers: a shared
`TRIGGERED` state machine and the dropper/dispenser boundary
(`getDispenseMethod`), dispatching per **item**, not per block — vanilla
registers roughly a dozen distinct dispense behaviours plus three implicit
defaults, and most of the exotic ones (minecarts, TNT, projectiles, shears,
armor) remain unmodelled because each needs an entity or mechanism this crate
does not have at all (a full enumeration with its own citation lives in
`redstone_dispenser.rs`'s module doc). Note blocks: per-block instrument
selection (9 single-block overrides, 7 heads, the small snare family; the two
largest families, bass/basedrum, are ~330 blocks and unmodelled) and the
rising-edge pulse; right-click cycling and the sound/particle pulse itself
are not wired. Tripwire: the full scan/attach/power algorithm and its 10-tick
recheck, wired for **placement** (vanilla drives this from `setPlacedBy`, not
`neighborChanged`); entity-crossing detection and the instant break-pulse
both need machinery (an entity-AABB census; a block-removal callback carrying
the destroyed state) this crate does not have yet. Target: hit-distance to
strength (`1..=15`, with a `max(1, …)` floor easy to drop on a grazing hit),
the arrow-vs-other duration split, and now a real producer — a resolved
projectile impact writes the new power and schedules its own decay.

Two general lessons from building this batch: a conjunction with an *entity*
half (detector rail wants only minecarts; tripwire wants any non-ignoring
entity) is not "half done," it is two separate features gated on two
different prerequisites, neither guessable from the other's shape. And a
device's trigger is not always a neighbour notification — a rail also has to
notify *itself* once on placement, and tripwire has no `neighborChanged` at
all, only placement and a self-scheduled poll.

### Live-oracle traps (if you build a redstone gate against a real server)

`/setblock` does **not** reproduce a power source's natural update fan-out
(that runs from a lever's own use/removal handler, not from `setBlock`) — use
redstone dust as the trigger instead, since its evaluator always does the
real fan-out. `pause-when-empty-seconds` defaults to 60; with no player
connected the world pauses and **no scheduled block tick ever fires again**,
so torches/repeaters/comparators/observers all look permanently dead while
dust (synchronous) keeps working — set it to 0. `/tick step N` **does**
advance scheduled block ticks; `/tick sprint N` returns immediately and a
following `/tick unfreeze` can interrupt it before it has run much of
anything.

### The beacon

Server-side pyramid detection, primary/secondary power selection, and
periodic effect application. `beacon_levels` computes the pyramid tier
(`0..=4`): each step's whole square of one of the five base-block types
**and every layer above it** must hold, so a broken layer 2 caps the result
at 1 even if layers 3–4 would otherwise qualify. `beam_unobstructed`
approximates vanilla's segmented-beam tracking (which this crate does not
render server-side) as "every block from directly above the beacon to a
fixed scan height is beam-transparent," reading the real per-state light-
dampening census rather than a hand-picked block list, so any genuinely
low-opacity block (a carpet, a candle) agrees with vanilla. `levels` is
refreshed only when the menu opens or a power is submitted, **not**
continuously — a menu left open while the pyramid is dismantled shows a stale
number until the next refresh, though effect *application* always recomputes
live and cannot outlive a broken pyramid. Effects apply every 80 game ticks,
matching vanilla's own `gameTime % 80 == 0` cadence, per connection (since
the wire notification and active-effect state are per-connection).

### The vibration substrate and the warden

A world-event type (`VibrationEvent`, modelled as exactly
`#warden_can_listen`'s tag members) plus a host-side "nearest audible event"
resolution with **no travel delay and no occlusion** — a disclosed
simplification, since vanilla's real signal walks toward a listener over
several ticks and can be blocked by intervening blocks. `MobSim` logs posted
vibrations per tick and resolves each listener species' nearest answer once,
at the **end** of the tick (deliberately after mob death-reaping, so a death
this same tick is audible this same tick). The one producer today is a dying
mob's own `EntityDie` event; nothing yet posts a vibration whose source is a
player, so a warden's anger/pursuit/attack target is always another mob.

The warden consumer (`mobs/warden.rs`) is close to complete: an emerge window
(134 ticks, invulnerable and unstrikeable), anger accumulation and decay,
single-suspect target tracking (a new vibration source replaces the tracked
target outright, unlike vanilla's multi-suspect tracking), real pursuit via
the Brain system, and two real attacks (a ranged sonic boom with its own
range/cooldown, falling back to melee) — all through the same damage
pipeline every other hit in this crate uses. Only `Digging` (the
give-up-and-despawn retreat) is left open, deliberately: its trigger depends
on a memory-module's *initial* state that the decompile alone could not
settle, and guessing it risks either every idle warden vanishing within
seconds or the behaviour never firing at all.

## How to change it, and the gotchas

- **Never reason about `FACING` as an output direction** — see "The signal
  model" above; every wrong test in the original landing made this mistake.
- Adding a new power source: extend `own_signal`/`weak_signal`/
  `direct_signal`/`is_signal_source` together, reading the block class's own
  `getSignal`/`getDirectSignal` rather than copying a neighbouring family's —
  the two are independent, and a weak-only implementation can pass nearly
  every gate while failing the one that actually depends on strong power.
- **Do not re-narrow the dust cascade back to one centre** — the second layer
  is load-bearing (see "Dust, torches, diodes, observers" above), and a rig
  that seeds dust at its already-settled power rather than a real source
  becomes vacuous the moment the second layer is live: the cascade correctly
  recomputes it to zero and correctly leaves everything downstream
  unpowered, which reads as a passing test proving nothing.
- Changing the piston commit encoding means changing both `finish_kind` and
  `parse_finish_kind` together, and `parse_finish_kind` must keep declining
  every kind it did not write.
- A new redstone-adjacent GPU/world borrow, a new dispenser item behaviour, or
  a new note-block instrument override: each has its own enumerated table in
  the relevant module's doc comment — read it before assuming "the family
  exists" means a specific item/block is covered.

## Configuration

No feature flags. Constants are vanilla's own: torch/comparator/observer
delay 2 ticks, repeater delay `2d`, rail search cap 8 cells, tripwire recheck
10 ticks (max span 41 cells), dispenser fire delay 4 ticks, target decay 20/8
ticks (arrow/other), beacon effect cadence 80 ticks.

## Dependencies

`crate::neighbor_update::{Direction, NeighborPropagator, ALL_DIRECTIONS}` —
the six-direction primitives every query and cascade builds on;
`crate::scheduled_tick::{ScheduledTickQueue, TickPriority}` for delayed
rechecks; `crate::chunk::{ChunkColumn, is_air_or_fluid}` for the world
representation every `lookup` closure reads through; `lodestone_data::block_entity_types`
for the piston/hopper "has a block entity" test;
`crate::mob_effects::ActiveEffects` and `lodestone_data::mob_effects` for the
beacon's effect grants; `lodestone_entity::vibration` and
`crate::mobs::warden` for the vibration substrate. See
[`docs/blocks.md`](./blocks.md) for the placement conventions that intercept
the three redstone-diode families before this module ever sees them.
