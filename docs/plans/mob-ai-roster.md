# Mob AI roster: species behavior and production wiring

## What it is

This document defines how species-specific behavior is assembled on top of `lodestone-entity` goal
and brain systems, then made observable through `lodestone-server`. A roster is correct only when it
is selected for a spawned species, ticked by production simulation, and its state changes reach a
connected client when the behavior is visible.

The roster separates reusable behavior primitives from species policy. Goal implementations express
movement, targeting, interaction, and timing; roster modules select those goals and priorities for a
species; `MobSim` supplies live perception and applies the resulting actions.

## How it works

`MobController` is the interface between behavior code and simulation state. Its production
implementation must supply live values for movement environment, idle time, nearby players,
attacker, tempting item, threat, panic state, breeding candidates, and parent candidates. A default
trait value is not evidence that a behavior works: a goal gated by an unfed value is an island even
when its unit tests pass against a scripted controller.

`lodestone_entity::ai::roster::goals_for` is the boundary for goal-driven species. It returns a
species-specific, priority-ordered collection of goals; an unknown species returns an empty
collection so fallback behavior is explicit. Keep species-family policy in separate roster modules,
not in `MobSim`, to preserve file-disjoint changes and make the policy testable without a world.

Brain-driven species use the same production path: `MobSim::spawn_species` selects their brain
composition, and simulation ticks it through a `BrainGoal`. The test must create a real simulated
mob and demonstrate a memory-to-action hand-off, such as a walk target producing movement. Testing
the brain in isolation does not prove the driver is connected.

The production chain is:

```text
spawn source -> MobSim::spawn_species -> roster selection -> goal or brain tick
    -> MobController action -> simulation state -> entity update or clientbound packet
```

Test every new roster entry across this chain. Include a negative control that removes the required
perception input or production registration and observes that the behavior does not occur. For
population-affecting behavior, assert the resulting entity count or spawned child rather than an
intermediate flag.

### Current production ledger

The production chain is present: `MobSim::spawn_species` selects the goal or
brain roster, the simulation ticks it, and natural spawning and spawn eggs use
that same species-aware entry point. `NavigatingMob` supplies the eight
perception values that must not fall back to trait defaults: water, lava, idle
time, nearest player, recent attacker, temptation, avoidance threat, and panic.
`MobSim::tick` supplies those values and resolves breeding/parent candidates,
so roster work must test the feed and action rather than only the goal object.

The remaining ledger is narrower than a presence check:

| surface | current result | remaining boundary |
|---|---|---|
| goal rosters and brain driver | selected from `spawn_species` and ticked in production | a species still needs its own behavior package and end-to-end gate |
| natural spawning | production cycle calls the same species path | placement rules remain server-owned data, not a version-free trait |
| version-free spawn seam | `SpawnEnvironment` was removed because it had no usable implementor | keep placement rules in `natural_spawn::SpawnRule`; do not revive a second engine without a real consumer |
| spawner blocks | block-entity ticking materializes attempts through `spawn_species` | species placement, custom spawn data/equipment, fixed positions, entity collision, and passengers are not modeled |
| camel | rider jump reaches dash state and metadata | ridden-mob physics remains client-authoritative; the server supplies no dash impulse or complete saddle/on-ground gate |
| sniffer | host driver seeks, walks, digs, rises, and drops the digging loot | cosmetic scent/happy states, path-aware target selection, particles/sounds/drop offset, and the full mid-dig cancellation set remain omitted |

### Dependency sequence

Keep the sequence visible even when individual components are already present:

```text
A perception inputs and simulation feeds
    -> B file-disjoint goal rosters
    -> C brain driver and brain rosters
    -> D population sources and placement rules
    -> E regional-difficulty policy
```

Perception precedes every roster because an unfed controller value makes a
correctly constructed goal inert. A driver precedes population-dependent
behaviour because a brain or goal with no production tick has no observable
consumer. Population sources may then create the same species path used by
tests; regional difficulty can remain independent until a spawn or despawn
decision consumes it.

Two goal-selector invariants matter for dynamic behavior:

- Goal and target selection have independent priority namespaces. If one selector represents both,
  the conversion must be documented and tested so target priorities cannot accidentally reorder
  movement goals.
- A behavior that changes equipment or state at runtime needs a supported remove/replace operation;
  disabling a broad flag is not an equivalent replacement.

Species behavior belongs in the following families:

| family | examples of responsibilities | primary seam |
|---|---|---|
| hostile melee | pursuit, melee attack, explosive approach, daylight response | goal roster and combat action |
| passive herd | panic, temptation, breeding, parent following, grazing | perception feeds and generated food data |
| ranged | charge, projectile launch, hit handling, equipment-dependent replacement | goal selector and projectile path |
| neutral | anger, retaliation, group alerting, delayed self-removal | shared anger state and entity updates |
| specialists | beams, volleys, explosions, sensors, multi-step attacks | dedicated state machine and packet metadata |
| brain-driven | memories, activities, schedules, and sensors | brain driver and population source |

Natural spawning, explicit spawning, and item-driven spawning must all call the same
species-aware creation path. A zombie-only convenience constructor, or a generic constructor that
installs no roster, is not a substitute for `spawn_species`.

## How to change it

Add a behavior primitive only when it applies to more than one roster or is a stable simulation
concept. Otherwise add the policy to the relevant `ai/roster/` or `brain/roster/` module and register
it through the roster boundary. Keep server-side patches limited to perception feeds, action drains,
and the species-aware spawn path.

Before relying on a capability, search for its production assignment as well as its trait method.
For example, a breeding goal requires a partner feed and a consumer that creates the child; a ranged
goal requires projectile creation, ticking, collision, damage, and wire visibility. Verify the full
chain rather than inferring it from a method name.

Obtain behavior constants and species data from the project's external oracle process or generated
data, then record the source in the implementation test. Do not derive expected values by
round-tripping Lodestone's own encoder or by copying a sibling roster. Generate extensible item or
tag tables under the repository's generate-or-assert workflow rather than maintaining hand-written
subsets.

When a behavior changes entity metadata, run
`crates/versions/26.2/oracle-java/EntityDataIndexOracle.java`. Never hand-count metadata indices.
Choose a type guard that distinguishes every colliding entity family; a shared byte layout does not
imply shared semantics. Use the generic metadata encoder rather than introducing a packet encoder
for one species.

Keep live gates realistic: allow a newly created entity a server tick before testing selector
visibility, use a target that remains targetable, and advance entity physics with the simulation
operation that actually executes physics. A control configuration must demonstrate that the gate
can fail.

## Configuration

Roster behavior is controlled by species data, goal priorities, simulation step size, and the
generated data tables consumed by the server. Data regeneration follows the repository's
`LODESTONE_REGEN=1` convention. Live gates use the configured integrated-server and client-oracle
workflow; they are intentionally separate from ordinary unit tests.

## Dependencies

The roster depends on `lodestone-entity` goals, brains, spawn definitions, and metadata types;
`lodestone-server` `MobSim`, the tick loop, spawn sources, projectile handling, and entity packet
output; `lodestone-data` entity classification; and the version-family metadata oracle. The broader
server integration rules are in [`server-ecs-migration.md`](server-ecs-migration.md), and protocol
delivery is constrained by [`multi-protocol-seam.md`](../multi-protocol-seam.md).
