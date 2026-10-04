# Dimension runtime

## What it is

The world owns one memoized entity runtime per hosted dimension and one connected-player
registry. A dimension's tick task, connection actions, and entity stream share its `MobHandle`
and `LiveMobSource`, including after travel.

## How it works

`WorldStateHandle` carries an `Arc<WorldRuntime>` outside its persisted scalar state. Three
`OnceLock<Arc<DimensionRuntime>>` slots cover the Overworld, Nether, and End. Each runtime
contains only its dimension tag, mutable mob simulation, and entity publication; it retains
no terrain source, world handle, or task. Constructors install the existing primary handles
before exposing sibling factories. Lazy sibling creation reuses `ensure_dimension_runtime`.

Fresh simulations use the destination's vertical geometry and start entity allocation at
1000. Network entity IDs and publication revisions belong to a dimension, so equal values
in two dimensions are legitimate. Both connection loops resolve their active mob handle and
entity source from the current dimension. Combat, item pickups, summons, entity queries,
boss bars, and streaming therefore reach the same population the dimension's tick task owns.
Compatibility connection wrappers without an installed runtime retain their supplied handles.

The canonical `PlayerRegistry` records identity, dimension, position, rotation, and inventory.
Each tick rebuilds the complete same-dimension perception list, so an idle join, a selected
item change, a terrain seed replacement, or disconnect does not depend on another movement
packet. `TickAnchors` derives the complete registry roster rather than accepting a last-mover
replacement. Manual anchors remain available to fixtures with no registered players. Tab-list
entries span the world; entity snapshots exclude the viewer and players in other dimensions.
The authoritative roster also clears departed riders from mobs, boats, and minecarts, even
when its last player leaves. Compatibility perception without identity remains separate from
this disconnect signal.

Dimension travel atomically republishes the player's position and dimension. It clears both
entity diff maps and the consumed publication revision, and removes old boss bars before
clearing their bookkeeping. Death-screen returns use the same reset. The destination must
send fresh entity additions even when its first revision and entity IDs match the origin.
End fight preparation resolves the End runtime before creating its population.

Chunk tickets follow the same source boundary. A restored join grants its loading/simulation
pair directly in the selected source's store. Portal arrival and both native/browser death-screen
returns share a pending destination lease and synchronous adoption helper in `connection_travel`.
The old pair remains owned during awaited delivery; cancellation removes the pending pair.
Adoption withdraws the old pair before publishing destination presence. Equal chunk coordinates
in different dimensions still require transfer because their ticket stores are distinct. The
home spawn's loading ticket is refreshed through a fixed home handle, independently of travel.
These tickets do not yet establish an active-visible/full entity lifecycle snapshot.

The primary tick loop is the clock `Owner`; sibling loops are `Follower`s. Only the owner
advances global time, consumes weather commands, advances weather, and applies sleep clock
changes. Followers read the shared clock for scheduled ticks and spawning. Each mob's
despawn check measures its nearest player from the dimension's complete perception list.

`dimension_tick::tests::bounded_natural_spawning_uses_stationary_presence_in_every_dimension`
executes the shared production tick body on virtual time: the primary ECS path for the
Overworld and follower paths for Nether and End. Each authored territory contains exactly
49 retained Full columns behind a real `ChunkStore` and loading/simulation ticket pair.
A foreign-dimension player leaves spawning empty during ten completed ticks; a stationary
matching player must then produce naturally selected mobs within 400 ticks. The fixture
checks resident admission, unchanged generation count, category census, species and ground
placement, and shared runtime/publication identity. Generation candidates and unrelated
spawn producers are absent or disabled. This is authored-terrain readiness/publication
evidence; protocol-776 streaming is covered separately, not one continuous natural-to-wire
test or evidence of generated-world rendering.

### Per-dimension gameplay rules

`Dimension::ultrawarm`, `Dimension::piglin_safe` and `Dimension::respawn_anchor_works` are the single source for rules that differ by hosted dimension (each is true only in the Nether). Readers: `FluidEnv::for_dimension` (fast lava), the dispenser water-bucket arm in `tick.rs` (water evaporates instead of placing), and `MobSim::set_piglin_safe`, which the tick loop feeds each tick so piglins, piglin brutes and hoglins zombify after 300 consecutive ticks outside a safe dimension. Add a new per-dimension rule as another `const fn` there and read it from the loop, rather than matching on `Dimension::Nether` at the call site.

## How to change it

Add a dimension by extending the fixed slot mapping and the dimension geometry together.
Keep installation ahead of every constructor path that can resolve a sibling. Pass runtime
handles to the tick loop and resolve connection handles at the current source boundary;
adding a private simulation or publication at either end breaks ownership.

Keep `ChunkSource::ticket_store` forwarded through `DimensionalSource` and pointer wrappers.
Resolve it only when selecting a join or travel destination, never from the per-tick path. A
ticket-backed home cannot use a sibling lacking this capability; no-ticket compatibility sources
retain their isolated connection handle. Ticket ownership is separate from terrain retention,
End fight state, and entity tracking.

`DimensionRuntime::publish_entities` lets a host publish after mutating its simulation. A
connection must forget its publication cursor whenever it changes source. Keep player roster
and entity snapshot filtering within one registry lock so profile entries precede additions.

Navigation still uses a bounded `ChunkWorld` snapshot. Lazy sibling runtimes begin with empty
navigation geometry; live collision and natural spawning read their dimension's authoritative
source. Rolling resident navigation and sibling entity persistence are separate milestones.
Do not refresh navigation with periodic `MobHandle::replace_world`: replacement discards the
population and retains another leaked snapshot. The existing primary terrain seeding remains
a one-time initialization operation.

## Configuration

There is no runtime switch. `Dimension::min_y` and `Dimension::height` define simulation bounds.
`TickFollow::radius` controls the followed area. Game rules continue to control natural spawning,
weather, and time; sibling creation does not add an extra global clock advance.

## Dependencies

- `world_state` owns runtime lifetime alongside shared scalars.
- `dimension` and `integrated` construct terrain sources and start dimension tasks.
- `mobs`, `players`, `tick_area`, and `tick` supply simulation, presence, area, and progression.
- `server` and `connection_travel` select the active runtime and maintain connection diff state.
