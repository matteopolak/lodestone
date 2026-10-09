# Redstone: dust, torches, repeaters, comparators, pistons, devices and the beacon

## What it is

The server's redstone model: dust and torch signal propagation, repeaters, comparators, observers, pistons, the input devices (levers, buttons, plates, rails, dispensers, note blocks, tripwire, target blocks), and two consumers built on top: the beacon and the vibration/warden substrate. Signal queries use `RedstoneLookup`, which reads typed `StateId` values and position-specific comparator output; production queries use a borrowed column view and the block-entity registry, and state-only closures adapt with zero comparator output.

## How it works

### Typed state values

Decisions keep a `lodestone_data::block_states::BlockStateValue` until they cross a text-facing event or storage boundary. A value from the generated table carries its `StateId` projection and original spelling; an unknown or invalid extension stays explicit text rather than a default state (the general state lookup deliberately accepts a forgiving default, so the strict property check matters). Placement results, gravity hand-offs and neighbour reactions use it internally and convert to text only at event feeds and world writes, preserving abbreviated property sets. NBT, schematic, datapack and import/export formats stay textual with their lossy-import diagnostics.

### The signal model

- With no collision-shape system, a redstone conductor is "anything solid that is not a redstone component", correct for what worldgen places and not the same as "a full cube": `minecraft:target` and `minecraft:redstone_block` are sources that still count as conductors because both register a full collision cube in the jar.
- Every `direction` parameter means the direction travelled from the querying position to reach the neighbour holding `state`. Reading a diode's `FACING` as the output direction rather than the side it reads from is the single most repeated mistake: `FACING` is the input side and output leaves the opposite face.
- Input sources (lever, button, pressure plate, weighted plate, tripwire hook, detector rail, target, daylight detector, redstone block) all emit weakly in all six directions (none overrides the strong-signal query), unlike every relaying family (torch, diode, observer), each of which excludes at least one direction. `target`, `daylight_detector` and `redstone_block` send no strong power at all: a block of redstone reaches a comparator's side input only through one explicit branch before the ordinary wire check, and missing it makes the block look unable to feed one.
- Some sources are wired for reads with no producer yet (nothing writes `powered`/`power`): pressure and weighted plates and detector rails need an entity-AABB census and the daylight detector a sky-light read. They sit at 0, a correct read with a missing producer. Tripwire and target now have producers (below).

### Dust, torches, diodes, observers

Dust's rule that a wire never counts an adjacent wire's current power as its own source has to apply to both the weak and strong signal queries, because the reference implements it as one shared flag on the single wire instance. Missing the strong half lets a wire on a conductor relay a second wire's power past the usual `-1` decay.

The cascade dust triggers is two layers deep: a wire's power change fans out through seven centres (its own position plus each neighbour), each getting a full six-direction notification, duplicates included (the reference dedupes centres, not notifications). One layer misses ordinary torch-inverter geometry (dust on a block, a torch on that block's side, a neighbour of a neighbour). Torches, repeaters, comparators and observers schedule a delayed recheck when steady state disagrees rather than mutating immediately; scheduled ticks drain through the same loop as a random tick, so a chain resolves depth-first within one drain.

Delay constants (live-oracle-measured): torch, comparator, observer 2 game ticks; repeater `2 * d` (2/4/6/8), same on both edges; observer pulse 2 ticks wide, starting on tick 2. A `powered=true` comparator does not lock a repeater by itself: a diode's lock contribution is its output signal (a comparator's stored analog output), so a freshly placed `comparator[powered=true]` has output 0 and locks nothing.

Comparator analog output lives in the typed comparator block-entity sidecar, not a block-state property. The scheduled tick's reaction carries the new output through the world-tick commit, updates the sidecar and emits a block-entity-data effect with numeric `OutputSignal`; reads query the sidecar by position; chunk NBT saves and restores it; new sidecars start at 0.

### A partial state string was delivered as zero

`resolve_state_id` matches by exact property set, and `redstone_wire`'s writer emits only `[power=N]` (one of five properties), so no dust state matched and the encoder fell back to the lowest id with that name, `power=0`. Every dust change the server sent was delivered to the client as zero while a fully connected wire computed the right value, invisible to any connectivity scan. The fix belongs in the encoder (subset match), not in what this crate computes.

### Placement triggers redstone

Callers of the propagation entry point are a random tick that mutated a block, the scheduled-tick drain, and `propagate_placement`, which runs the same fan-out and persists every change with a real `BLOCK_UPDATE`. It resolves only the synchronous half: dust completes (0 ticks), while a torch, repeater, comparator or observer reacts by scheduling a recheck the tick loop owns, and does nothing when placed next to a live circuit because the placement path's queue is local and discarded.

### Pistons

`crate::piston` ports the base block and structure resolver as pure decisions:
- push reactions per block (destroy, block, push-only, normal), four hard-coded unpushable exceptions, and any block entity;
- the resolver: a 12-block search with order-sensitive sticky reordering at a collision (slime and honey do not stick to each other, each sticks to everything else);
- quasi-connectivity: `has_extend_signal` is not `best_neighbor_signal`: it excludes the push direction at the neighbour cell and also reads the cell above the piston in every direction except down, a quirk BUD switches and observer clocks depend on;
- two-phase animated move: `begin_move` splits the one-step write into "empties now" and "holds a `moving_piston` placeholder for 2 ticks", and `finish_move` replays the one-step writes rather than recomputing, so the committed world is byte-identical to a one-step push.

Timing: a move starts on tick `N` and commits on `N + 2`. A placement at counter boundary `N` queues a zero-delay `ScheduledTickKind::Piston` recheck; the ingress batch rebases after the next increment, so it starts at `N + 1` and commits at `N + 3`. The first two updates ramp by 0.5 and the third commits (completion is tested before ramping). Notifications already inside a world tick run in its move phase without the ingress delay. Four cells animate on a three-block push (the arm cell is a travelling block carrying the head).

- The moving-block record travels as a scheduled tick's encoded kind string (no block-entity map on the reaction surface), so match by prefix, never equality. Its carried state keeps the exact runtime spelling for the final write, including server-only properties such as a comparator's `output=N`; an optional validated `StateId` projects the built-in portion for moving-block NBT, and data-pack states without it still animate and commit verbatim.
- A client needs two packets per animating cell, in order: `block_update` writing `minecraft:moving_piston[...]` (making the client create the record), then `block_entity_data` with the moving state's save tag. Either alone draws nothing. Silent traps: `facing` is a byte in declaration order, not alphabetical (an `Int` decodes as absent and everything animates down); `progress` must be the value before this tick's `+0.5` ramp since the client owns the ramp; the block's registry key (`moving_piston`) differs from its block-entity key (`piston`).
- A move is interruptible: retracting checks the piston's own arm cell for a pending commit from its own extend and forces it to finish, and a narrower check covers the cell a sticky retraction would pull from (possibly another piston's animating extension). A push cannot cross a chunk border because the shared lookup reads air outside its 16x16 footprint (a property of the whole family).

### Devices

Five pure-decision modules in the same shape:
- Rails: `POWERED` tracking for powered and activator rails (one class registered twice); only the activator's minecart launching is unmodelled (no minecarts); curve connectivity is a placement concern.
- Dispensers and droppers: a shared `TRIGGERED` state machine and per-item dispatch (about a dozen behaviours plus three implicit defaults; the exotic ones, such as minecarts, TNT, projectiles, shears and armour, are unmodelled; the full list is in `redstone_dispenser.rs`'s module doc).
- Note blocks: per-block instrument selection (9 single-block overrides, 7 heads, the small snare family; the bass and basedrum families, about 330 blocks, unmodelled), rising-edge pulses, hand-use pitch cycling through 25 values with wraparound; audible sound and particles need a client-visible block-action message.
- Tripwire: full scan, attach and power algorithm with a 10-tick recheck, driven from placement (not neighbour changes); entity-crossing detection and the instant break pulse need an entity-AABB census and a block-removal callback carrying the destroyed state.
- Target: hit distance to strength (`1..=15`, with a `max(1, ...)` floor easy to drop on a grazing hit), the arrow-versus-other duration split, and a real producer (a resolved projectile impact writes the power and schedules its decay).

Lessons: a conjunction with an entity half (detector rail wants only minecarts, tripwire any non-ignoring entity) is two features with different prerequisites. A device's trigger is not always a neighbour notification: a rail notifies itself once on placement, and tripwire has no neighbour-change hook, only placement and a self-scheduled poll.

### Live-oracle traps

- `/setblock` does not reproduce a power source's natural update fan-out (that runs from the lever's use handler); use redstone dust as the trigger.
- `pause-when-empty-seconds` defaults to 60: with no player connected the world pauses and no scheduled block tick fires, so torches, repeaters, comparators and observers look dead while synchronous dust works. Set it to 0.
- `/tick step N` advances scheduled block ticks; `/tick sprint N` returns immediately and a following `/tick unfreeze` can interrupt it.

### The beacon

`beacon_levels` computes the pyramid tier (`0..=4`): each step's whole square of one of the five base-block types and every layer above it must hold, so a broken layer 2 caps at 1. `beam_unobstructed` approximates segmented-beam tracking (not rendered server-side) as "every block from above the beacon to a fixed scan height is beam-transparent", using the per-state light-dampening census so low-opacity blocks (carpet, candle) agree. `levels` refreshes only when the menu opens or a power is submitted, so an open menu shows a stale number after dismantling, while effect application always recomputes live. Effects apply every 80 game ticks, per connection.

### The vibration substrate and the warden

`VibrationEvent` is exactly the members of the `#warden_can_listen` tag, with host-side nearest-audible-event resolution and no travel delay or occlusion (disclosed simplification; the real signal travels over several ticks and can be blocked). `MobSim` logs vibrations per tick and resolves each listener species' nearest at the end of the tick, after death-reaping, so a death this tick is audible this tick. The one producer is a dying mob's `EntityDie`; nothing posts a vibration sourced by a player, so a warden's target is always another mob.

`mobs/warden.rs` is close to complete: a 134-tick emerge window (invulnerable, unstrikeable), anger accumulation and decay, single-suspect tracking (a new source replaces the target outright), real pursuit via the Brain system, and two attacks (ranged sonic boom with its own range and cooldown, falling back to melee) through the shared damage pipeline. Only `Digging` (give-up-and-despawn retreat) is open: its trigger depends on a memory module's initial state the decompile did not settle, and a guess risks idle wardens vanishing in seconds or never firing.

## How to change it

- Never reason about `FACING` as an output direction.
- New power source: extend `own_signal`, `weak_signal`, `direct_signal` and `is_signal_source` together, from the block's own two signal queries rather than a neighbouring family's; a weak-only version can pass nearly every gate and fail the one needing strong power.
- Do not narrow the dust cascade to one centre. A rig seeding dust at its already-settled power becomes vacuous once the second layer is live (the cascade correctly recomputes it to zero and leaves downstream unpowered, reading as a passing test proving nothing).
- Changing the piston commit encoding means changing `finish_kind` and `parse_finish_kind` together; the parser must keep declining every kind it did not write.
- A new dispenser item behaviour, note-block instrument override or redstone-adjacent GPU/world borrow has an enumerated table in the relevant module doc; read it before assuming coverage.

## Configuration

No flags. Constants: torch/comparator/observer delay 2 ticks, repeater `2d`, rail search cap 8 cells, tripwire recheck 10 ticks (max span 41 cells), dispenser fire delay 4 ticks, target decay 20/8 ticks (arrow/other), beacon cadence 80 ticks.

## Dependencies

`crate::neighbor_update::{Direction, NeighborPropagator, ALL_DIRECTIONS}`; `crate::scheduled_tick::{ScheduledTickQueue, TickPriority}`; `crate::chunk::{ChunkColumn, is_air_or_fluid}`; `lodestone_data::block_entity_types` (piston/hopper block-entity test); `crate::mob_effects::ActiveEffects` and `lodestone_data::mob_effects` (beacon grants); `lodestone_entity::vibration` and `crate::mobs::warden`. See [`blocks.md`](./blocks.md) for the placement conventions intercepting the three diode families.
