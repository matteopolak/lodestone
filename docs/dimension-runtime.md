# Dimension runtime

## What it is

The world owns one memoized entity runtime per hosted dimension and one connected-player registry. A dimension's tick task, connection actions and entity stream share its `MobHandle` and `LiveMobSource`, including after travel.

## How it works

- `WorldStateHandle` carries an `Arc<WorldRuntime>` outside its persisted scalars, with three `OnceLock<Arc<DimensionRuntime>>` slots (Overworld, Nether, End). A runtime holds only its dimension tag, mutable mob simulation and entity publication, no terrain source, world handle or task. Constructors install the primary handles before exposing sibling factories; lazy siblings go through `ensure_dimension_runtime`.
- Fresh simulations use the destination's vertical geometry and allocate entity IDs from 1000. Entity IDs and publication revisions are per dimension, so equal values in two dimensions are legitimate. Both connection loops resolve mob handle and entity source from the current dimension, so combat, pickups, summons, queries, boss bars and streaming reach the population the tick task owns. Compatibility wrappers without an installed runtime keep their supplied handles.
- The canonical `PlayerRegistry` records identity, dimension, position, rotation and inventory. Each tick rebuilds the full same-dimension perception list, so joins, item changes, seed replacement and disconnects need no further movement packet. `TickAnchors` derives from the whole roster (manual anchors remain for fixtures without players). Tab-list entries span the world; entity snapshots exclude the viewer and other dimensions' players. The roster also clears departed riders from mobs, boats and minecarts, even when the last player leaves.
- Dimension travel atomically republishes position and dimension, clears both entity diff maps and the consumed publication revision, and removes old boss bars before clearing bookkeeping; death-screen returns do the same. The destination must send fresh additions even if its first revision and IDs match the origin. End fight preparation resolves the End runtime first.
- **Chunk tickets** follow the source boundary. A restored join grants its loading and simulation pair in the selected source's store. Portal arrival and both death-screen returns share a pending destination lease and synchronous adoption helper in `connection_travel`: the old pair stays owned during awaited delivery, cancellation removes the pending pair, and adoption withdraws the old pair before publishing destination presence. Equal coordinates in different dimensions still transfer because stores are distinct. The home spawn's loading ticket is refreshed through a fixed home handle independent of travel. Tickets do not yet establish an active-visible/full entity lifecycle snapshot.
- The primary tick loop is the clock `Owner`; siblings are `Follower`s. Only the owner advances global time, consumes weather commands, advances weather and applies sleep clock changes. Followers read the shared clock. A mob's despawn check uses the dimension's complete perception list.
- `dimension_tick::tests::bounded_natural_spawning_uses_stationary_presence_in_every_dimension` runs the production tick body on virtual time (primary ECS path for the Overworld, follower paths for Nether and End) over 49 retained Full columns behind a real `ChunkStore` and ticket pair: a foreign-dimension player leaves spawning empty for ten ticks, then a stationary matching player must produce natural spawns within 400 ticks. It is readiness and publication evidence for authored terrain, not a continuous natural-to-wire test.
- **Per-dimension rules.** `Dimension::ultrawarm`, `piglin_safe` and `respawn_anchor_works` (each true only in the Nether) are the single source. Readers: `FluidEnv::for_dimension` (fast lava), the dispenser water-bucket arm in `tick.rs` (water evaporates), and `MobSim::set_piglin_safe` (piglins, brutes and hoglins zombify after 300 consecutive ticks outside a safe dimension). Add a new rule as another `const fn` there rather than matching `Dimension::Nether` at the call site.

## How to change it

- A new dimension extends the fixed slot mapping and geometry together; installation stays ahead of any constructor path that resolves a sibling. Pass runtime handles to the tick loop and resolve connection handles at the current source boundary; a private simulation or publication at either end breaks ownership.
- Forward `ChunkSource::ticket_store` through `DimensionalSource` and pointer wrappers; resolve it only when picking a join or travel destination, never per tick. A ticket-backed home cannot use a sibling lacking it; no-ticket compatibility sources keep their isolated handle.
- `DimensionRuntime::publish_entities` lets a host publish after mutating its simulation. A connection must forget its publication cursor on any source change. Keep roster and entity snapshot filtering under one registry lock so profile entries precede additions.
- Navigation still uses a bounded `ChunkWorld` snapshot; sibling runtimes start with empty navigation geometry while live collision and spawning read the authoritative source. Do not refresh navigation with periodic `MobHandle::replace_world`: it discards the population and leaks another snapshot.

## Configuration

No runtime switch. `Dimension::min_y` and `height` set simulation bounds; `TickFollow::radius` sets the followed area; game rules govern spawning, weather and time. Sibling creation adds no extra clock advance.

## Dependencies

`world_state` (lifetime), `dimension` and `integrated` (terrain sources, tasks), `mobs`, `players`, `tick_area`, `tick` (simulation, presence, progression), `server` and `connection_travel` (active runtime, diff state).
