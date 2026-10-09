# World propagation

## What it is

Block-level effects that spread through the world over time on the integrated server: fire spread and burnout, water and lava flow, bubble-column push and pull, and nether portal formation and travel. Each ports the reference block-tick behaviour and runs off the scheduled-tick queue.

## How it works

### Fire

`crates/lodestone-server/src/fire.rs` ports the fire tick. Fire is not random-ticked; it schedules its own next tick, so a fire that loses its schedule is inert forever. The only producer of fresh fire is lava's random tick (fire has no item). Each tick draws from the RNG in a fixed order: reschedule delay, rain-out roll, age advance, age-15 self-extinguish, one draw per neighbour burn-out check (six neighbours, then `x`, `z`, `y` with `y` from -1 to 4, not a symmetric cube), then one draw per spread candidate over the 26-cell neighbourhood. A reordered or skipped draw gives a plausible but wrong world, so the sequence is the spec.

Odds use integer-truncating arithmetic: `(ignite_odds + 40 + difficulty*7) / (age + 30)`, `rate = 100 + max(0, dy-1)*100`, catching when a draw below `rate` is `<= odds`. Truncation is behaviour (oak planks land on the same 2-in-100 at fresh and max age, because `59/45` and `45/30` both truncate to `1`). Ignite and burn odds are not in `blocks.json`; they come from boot-time internal maps, reflected into the generated `lodestone_data::block_blast`. "Spreads to" odds and "ignited by lava" are different sets (every bed and note block ignites from lava with no spread odds; small flowers and hay and coal blocks are the reverse), so never derive one from the other. `#minecraft:infiniburn_overworld` is netherrack and magma block, not bedrock.

Burnout is per biome: a biome setting `gameplay/increased_fire_burnout` (jungle, swamp, mushroom fields, snowy peaks) burns neighbours out faster and spreads more readily. The scheduled-tick arm in `tick.rs` builds each fire's `FireEnv::in_biome(biome at the fire)` through `worldgen_data::increased_fire_burnout`; lightning and flint-and-steel placement environments do not read the flag.

### Fluids

`lodestone_server::fluid` handles scheduled spread: quench first (lava meeting water becomes obsidian, cobblestone or basalt), recompute a non-source cell from neighbours, then spread down, and sideways only if down is refused, toward the neighbour(s) at the shortest distance to a hole within a per-fluid search distance. Fluid spread draws no RNG; the one reference randomness (a tick-delay multiplier on deepening lava) is not modelled and affects only timing.

Kelp, seagrass, tall seagrass and bubble columns hold source water without a `waterlogged` property, so reads must treat them as occupied water or a nearby tick replaces underwater plants. Generated-tick admission still schedules only actual liquid blocks.

Reach is per fluid and dimension: water 7 cells, overworld lava 3, nether lava ticks sooner and goes less far. The block `level` (0..15) and the internal `amount`/`falling` state are two encodings that are easy to invert: `level=0` is a source, `1..=7` flowing (`amount = 8 - level`), `8..=15` a falling column at `amount = 8`; a falling cell is not a source (treating it as one makes a waterfall self-sustaining).

Every written cell reschedules itself and its fluid neighbours, which is how a flow drains when its source is removed (a fluid never re-evaluates by itself); a cell that quenches or empties must not return before the neighbour reschedule. Waterlogging fires only for a water source, keyed on the target's derived source state, not the water family: letting flowing water waterlog turns every waterloggable block into a source relay (measured 125x the fluid ticks and 107x the block writes on one fixture). Ticks operate on the fluid a block holds, including contained sources; waterlogged blocks keep their identity and collision while spreading; hydration and neighbouring edits schedule their own water tick.

Occupancy and waterlogging resolve through one typed table indexed by `StateId`, built once from the state corpus. Each slope search keeps its scratch in a fixed 11x11 grid (maximum four-step search plus the outer candidate) instead of hash maps; change the bound and the edge-case tests together.

### Bubble columns

`apply_bubble_column` (`crates/lodestone-physics/src/player.rs`) gives a vertical impulse when the player occupies or stands above a `bubble_column`: soul sand pushes up, magma block pulls down, stronger when the cell above is open air than when submerged. The four constants are asymmetric between directions and between inside and above, so "above" is not a uniform multiple. The block resolves soul sand versus magma once into one boolean property; physics reads only that.

The impulse applies after movement is integrated (tick 0 position equals plain water; only velocity and tick 1 diverge), and a standing player spans two cells and receives it once per overlapping cell, not per tick (once per tick converges to the same terminal velocity at half the rate).

### Nether portals

`lodestone_server::dimension` and `portal` hold dimension identity and geometry, and frame detection, ignition and destination search. `ChunkSource` has defaulted `dimension`, `sibling` and `portal_index` methods so a connection can shadow its source on travel, with every read following the player's current dimension. Flint and steel searches outward for a valid 2-21 wide, 3-21 tall empty frame. Standing in a lit portal for the game-rule number of ticks (post-increment comparison: 80 fires on the 81st tick) triggers travel: destination resolved with the 8:1 scale, columns prefetched in parallel, then a full chunk-forget, respawn and re-stream sequence, which any vanilla-protocol client needs even though the Lodestone client has local teardown.

Synchronous portal reads are resident-only. The return search describes candidates with `find_exit_portal_required_columns` and reads captured resident columns with `find_exit_portal_resident`; inexact End gateway contact uses `end_gateway_required_columns` and `end_gateway_arrival_in_resident_world`; End platform repair uses `end_platform_required_columns` and `ensure_end_platform_if_resident`. A missing footprint defers rather than generating inside a tick. Once resident, search order and tie-breaks are unchanged, so admission changes latency, not destination.

End portals reuse the counter and cooldown machinery but fire on the first tick, with no coordinate scale or search (a fixed platform). A 12-frame ring filled with eyes of ender, each facing the centre, opens one; there is no stronghold generator, so frames are hand-placed today. The return trip is the exit handshake in [nether portals](nether-portals.md).

## How to change it

- Every read in `fire.rs`/`fluid.rs` goes through a helper answering air outside build height (they read the cell below what they inspect; an unguarded floor read panics the tick thread).
- A per-tick ground, water or inside-block check must scan every integer cell the movement crossed, not just the destination (tunnelling at speed).
- Fire's rain check walks a full column each raining tick, deliberately unoptimised (a heightmap would be the fix).
- The face-occlusion predicate used by fluid spread is exact for neighbour-independent shapes and wrong for stairs, fences, walls and panes; it fails toward under-spreading. Do not loosen it: a leak through a wall is unrecoverable in a saved world.
- A new base block in either bubble-column tag needs no entity-side change.
- Portal frame detection and destination search are direct derivations of one fixed pattern, not the generalised multi-block matcher.

## Configuration

| system | knob | default | effect |
|---|---|---|---|
| fire | `fire_spread_radius_around_player` | 128 | `-1` disables fire; otherwise a player must be within range |
| fire | difficulty | n/a | scales odds via the `difficulty * 7` term |
| fire | `random_tick_speed` | 3 | how often lava can ignite fire |
| fluid | `water_source_conversion` / `lava_source_conversion` | true / false | read into `FluidEnv` at tick-loop build, not live |
| portals | `allow_entering_nether_using_portals` | true | gates travel into the Nether only |
| portals | `players_nether_portal_default_delay` / `..._creative_delay` | 80 / 0 | ticks standing in a lit portal |

Bubble-column constants are fixed.

## Dependencies

- `lodestone_data::{block_blast, block_solidity, collision_shapes, snow_support}`: odds and geometry tables generated from a real headless server.
- `crate::scheduled_tick`, `crate::chunk::ChunkSource`, `crate::mob_spawn::SpawnRng`.
- `lodestone-physics`/`lodestone-model` for the bubble-column seam (`CollisionView::bubble_column`, `VersionAdapter::block_bubble_column_drag`).
- `lodestone-worldgen`'s `nether` module, and `lodestone-v26-2`'s `server_protocol` for dimension-change encoding (the dimension to registry-holder mapping is a protocol-family property).
