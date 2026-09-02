# World propagation

## What it is

Block-level effects that spread through the world over time on the integrated server: fire spread and
burnout, water/lava flow, the vertical push/pull of bubble columns, and nether portal
formation/travel between dimensions. Each is a port of the corresponding vanilla block-tick behaviour,
driven by the same scheduled-tick queue.

## How it works

### Fire spread

`crates/lodestone-server/src/fire.rs` transcribes `FireBlock::tick` and its neighbours. Fire is **not**
random-ticked — it schedules its own next tick, so a fire block that ever loses its pending schedule is
inert forever; the only producer of a fresh fire is lava's random tick (fire has no item, and nothing
else ignites a block). Every tick draws from the RNG in a fixed order — reschedule delay, rain-out
roll, age advance, age-15 self-extinguish, then one draw per neighbour burn-out check (six neighbours,
`x`-then-`z`-then-`y`, `y` from -1 to 4, not a symmetric cube), then one draw per spread candidate over
the 26-cell neighbourhood. A reordered or skipped draw produces a plausible-looking but non-vanilla
world, so the sequence itself is the specification, not just the outcome.

Spread/burn odds come from integer-truncating arithmetic (`(igniteOdds + 40 + difficulty*7) / (age +
30)`, `rate = 100 + max(0, dy-1)*100`, catches when `nextInt(rate) <= odds`) — the truncation is what
gives fire its actual behaviour (e.g. oak planks land on the same 2-in-100 chance whether the fire is
fresh or at max age, because `59/45` truncates to the same `1` as `45/30`). Ignite/burn odds are not a
block property in vanilla's own data — they live in two internal maps populated at boot (`FireBlock`
bootstrap), reflected out into a generated table (`lodestone_data::block_blast`) rather than transcribed
from `blocks.json`, which carries no flammability field at all. `igniteOdds > 0` and `ignitedByLava` are
different sets, neither a subset of the other (every bed and note block ignites from lava with no spread
odds of its own; every small flower and hay/coal block is the reverse) — never derive one from the
other. `#minecraft:infiniburn_overworld` is netherrack and magma block, not bedrock.

### Fluid spread

`crates/lodestone-server/src/fluid.rs` ports the `FlowingFluid` family: quench first (lava meeting water
becomes obsidian/cobblestone/basalt), recompute a non-source cell from its neighbours, then spread down
first and sideways only when down is refused — sideways spread goes only toward the neighbour(s) at the
shortest distance to a hole, capped at a per-fluid search distance. Unlike fire, fluid spread draws no
RNG at all; the only randomness in the family is a tick-delay multiplier on deepening lava, which this
crate does not model (affects lava's timing while thickening, never its final shape).

Reach is fixed by a per-fluid, per-dimension drop-off: water reaches 7 cells from a source, overworld
lava reaches 3, nether lava (faster tick, larger drop-off) effectively less far but ticks sooner. The
block's `level` property (0..15) and the fluid's internal `amount`/`falling` state are two different
encodings of the same thing and are easy to invert: `level=0` is a source, `1..=7` is flowing (`amount =
8 - level`), and `8..=15` is a **falling** column at `amount = 8` — a falling cell is not a source, and
treating it as one makes a waterfall self-sustaining.

Every written cell must reschedule itself **and all six neighbours** — this is what lets a flow *drain*
when its source is removed, since a fluid cell never re-evaluates on its own and adjacent water can't
push a receding edge back. A cell that quenches or empties out must not return early before that
neighbour-reschedule runs, for the same reason. Waterlogging only fires for a water *source*, keyed on
Java-side reference identity between the flowing and source fluid singletons, not on the fluid family —
collapsing that distinction lets flowing water waterlog freely, which turns every waterloggable block
into a source relay and floods a scene roughly two orders of magnitude further and slower than vanilla
(measured: 125x the fluid ticks, 107x the block writes, on one otherwise-identical fixture). A
waterlogged block also does not *originate* a spread of its own in this port — a real limitation, never
producing more reach than vanilla, only occasionally less. No bucket item exists here, so a player's only
route to a new fluid source is world edits.

### Bubble columns

`crates/lodestone-physics/src/player.rs`'s `apply_bubble_column` applies a vertical velocity impulse
when the player occupies (or stands above) a `bubble_column` block: soul sand pushes up, magma block
pulls down, and the impulse is stronger when the cell directly above is open air than when it's
submerged. The four vanilla constants are asymmetric between the two directions and between the "inside"
and "above" cases — don't assume the "above" case is a uniform multiple of "inside" for both directions,
it isn't. The base-block resolution (soul sand vs. magma) happens once at the block level into a single
boolean property; the entity-side physics only ever reads that boolean.

Two easy-to-miss details: the impulse applies **after** movement is integrated for the tick (so tick 0's
*position* in a column is identical to plain water; only its *velocity*, and tick 1's position, diverge),
and a standing player spans two cells vertically and receives the impulse **once per overlapping cell**,
not once per tick — a port that applies it once per tick converges to the same terminal velocity at half
the rate, which is a real, observable divergence.

### Nether portals

`lodestone_server::dimension`/`portal` hold dimension identity/geometry and portal frame
detection/ignition/destination search respectively; a `ChunkSource` gained defaulted `dimension`/
`sibling`/`portal_index` methods so a connection can shadow its working source when a player travels,
with every read (chunk streaming, block reads, fall/drowning checks) automatically following the
player's current dimension. Igniting a frame with flint and steel searches outward from the adjacent
cell for a valid 2-21-wide, 3-21-tall empty frame; standing in a lit portal for a game-rule-configured
number of ticks (with a post-increment comparison — a delay of 80 fires on the 81st tick) triggers
travel, which resolves the destination (applying the fixed 8:1 overworld/Nether coordinate scale),
prefetches the destination's columns in parallel before the player arrives, and sends a full
chunk-forget/respawn/re-stream sequence so any vanilla-protocol client tears down the old dimension
correctly — this sequence is required even though the client keeps its own local teardown logic, since
it's the only thing a non-Lodestone client can act on.

End portals reuse the same per-tick counter/cooldown machinery but fire on the very first tick (the
Nether's delay is that block's own override of a default of zero) and have no coordinate scale or
destination search — the destination is a fixed platform. A 12-frame ring, correctly filled with eyes of
ender and each frame facing the ring's centre, opens the portal; there is no stronghold generator, so
reaching one today means hand-placing frames. The return trip (stepping into an end portal from inside
the End) is an intentional no-op — it needs the stronghold exit portal and the dragon fight, neither of
which exists yet.

## How to change it, and the gotchas

- Every world-propagation read in `fire.rs`/`fluid.rs` goes through a helper that answers air outside
  build height — the modules read the cell *below* whatever they inspect, so an unguarded read on the
  world floor panics the tick thread. Keep this invariant when extending either module.
- A per-tick "am I on the ground / in water / inside a block" check anywhere in this cluster must scan
  every integer cell an entity's movement crossed during the tick, not just sample the post-move
  position — sampling only the destination can tunnel through thin geometry at high speed.
- Fire's rain check walks a full column looking for a sky-blocking block on every raining tick; this is
  intentionally unoptimized (a heightmap would be the fix if it ever matters) rather than a looser test
  that would change behavior.
- A face-occlusion/collision predicate used by fluid spread is exact for a state whose shape does not
  depend on its neighbours, and *wrong* for one that does (stairs, fences, walls, panes) — it currently
  fails toward under-spreading, which is the safe direction; do not loosen it toward over-spreading, since
  a leak through a wall is unrecoverable in a saved world.
- A new base block added to either bubble-column tag needs no entity-side change — the server resolves
  drag direction once, at the block level, before physics ever sees it.
- Portal frame detection and destination search are not generalized into vanilla's reusable multi-block
  pattern matcher; each is a direct derivation of the one fixed pattern it needs.

## Configuration

| system | knob | default | effect |
|---|---|---|---|
| fire | `fire_spread_radius_around_player` (game rule) | 128 | `-1` disables fire entirely; otherwise a player must be within range |
| fire | difficulty | — | scales spread odds (`difficulty * 7` term) |
| fire | `random_tick_speed` (game rule) | 3 | how often lava gets a chance to ignite a fire |
| fluid | `water_source_conversion` / `lava_source_conversion` (game rules) | true / false | read into `FluidEnv` at tick-loop build time, not live yet |
| portals | `allow_entering_nether_using_portals` | true | gates travel *into* the Nether only |
| portals | `players_nether_portal_default_delay` / `..._creative_delay` | 80 / 0 | ticks required standing in a lit portal |

Bubble columns have no configuration — the four constants are fixed vanilla literals.

## Dependencies

- `lodestone_data::block_blast`, `block_solidity`, `collision_shapes`, `snow_support` — fire/fluid odds
  and geometry tables generated from a real headless server, not `blocks.json`.
- `crate::scheduled_tick`, `crate::chunk::ChunkSource`, `crate::mob_spawn::SpawnRng` — the tick queue,
  world access and RNG shared by fire and fluid.
- `lodestone-physics`/`lodestone-model` — the bubble-column collision seam
  (`CollisionView::bubble_column`, `VersionAdapter::block_bubble_column_drag`).
- `lodestone-worldgen`'s `nether` module — the Nether/End terrain generators portals travel into.
- `lodestone-v770`'s `server_protocol` — the wire encoding for dimension changes; the mapping from a
  dimension to its registry holder id is a property of the protocol family, not of `dimension`/`portal`
  themselves.

