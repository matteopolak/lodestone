# Server simulation — the roadmap

**Scope:** server-side world simulation: chunk lifecycle, persistence, block behaviour, redstone,
world state, the tick loop, and operational server plumbing. Command execution is a separate
subsystem. Mob AI, pathfinding, breeding, villagers, and raids belong to
[`server-entities.md`](./server-entities.md); this roadmap names their dependencies only where they
meet the world-simulation path.

## Foundations already in place

World generation, collision shapes, hardness, entity dimensions, and block-physics constants have
independent reference gates. The server has a transport-neutral connection loop, in-memory and TCP
transports, a registered 26.2 server protocol, an NBT reader/writer, and a 20 Hz tick loop with
MSPT/TPS accounting. A feature described below must reuse those owners rather than recreate them.

## Dependency edges

```
tick and protocol core
  ├── chunk residency ────────────────┐
  ├── persistence ◀───────────────────┘  (unload hands state to storage)
  ├── scheduled/block ticks ──→ redstone components
  ├── world state ──→ sleep, spawn location, dimension transfer
  └── operational services
```

Tick ownership and a served-client path are the common prerequisites. Chunk unloading and persistence
are coupled at their handoff; scheduled neighbour propagation is the shared primitive for block
reactions and redstone. World-state features are mostly independent, except that sleep consumes time,
weather, and rules, while spawn residency consumes the world spawn point.

The implementation order is therefore explicit: establish the server core first; develop chunk
residency and persistence together, but enable unload/autosave only after both handoffs exist; land
scheduled ticks and neighbour propagation before any redstone component; then add world-state
features, with sleep after time/weather/rules and spawn residency after world spawn. Remote console,
query, status, resource-pack, plugin-channel, and access work are parallel leaves once the host path
exists. Multi-dimension work is an exception: estimate it as a combined generation, storage, tick,
transfer, and stream feature rather than a small portal patch.

## Work inventory

### Server core

| feature | acceptance path |
|---|---|
| Unified server tick loop | A real server advances at 20 Hz and publishes observable time, mob, block-entity, and effect updates. |
| MSPT/TPS accounting | The tick owner reports work and overrun; a local timer cannot hide from the budget. |
| Shell singleplayer protocol wiring | The integrated server, server protocol, and shell form one join-to-render path, not a crate-test island. |

### Chunk lifecycle

| feature | acceptance path |
|---|---|
| Ticket and loading-priority system | Residency moves through the complete empty-to-full status pipeline. |
| View and simulation distance | Player movement changes the streamed and simulated areas. |
| Unload and save-on-unload | An unloaded column saves, reloads, and retains authoritative state. |
| Asynchronous generation | Generation never blocks the connection loop and its completed column reaches the stream. |
| Served carvers and ore features | A generated served chunk visibly contains the generated terrain features. |
| Spawn-area residency | The configured spawn area stays resident at the world spawn point. |

### Persistence

| feature | acceptance path |
|---|---|
| Region-file storage | A complete column, including relevant NBT, survives an independent read and server reload. |
| World metadata | World-level state loads and saves through the same owner the tick loop reads. |
| Player data | Per-player state returns through login and is never confused with world data. |
| Entity and point-of-interest storage | Per-chunk entities and POI occupancy survive the real persistence boundary. |
| Autosave and data-version handling | Periodic saves coordinate with tick ownership and preserve upgrade metadata. |

### Block behaviour

| feature | acceptance path |
|---|---|
| Random ticks | The tick loop selects and runs bounded random work under the world rule. |
| Scheduled ticks and neighbour propagation | A mutation queues due work and delivers bounded notifications in the shared order. |
| Fluid simulation | A fluid update changes authoritative block state and streams to the client. |
| Crop, sapling, and leaf behaviour | Growth and decay are driven by the common tick mechanisms. |
| Gravity blocks | Unsupported blocks become falling entities or settle through the live path. |
| Fire | Spread, burnout, and rule/range gates run from the shared scheduling path. |
| Block destruction from explosions | Blast resistance and destroyed blocks join entity exposure to one observable explosion. |

### Redstone

| feature | acceptance path |
|---|---|
| Dust and torch propagation | A source change propagates through the shared signal and neighbour model. |
| Repeaters and comparators | Delayed and analogue behaviour uses scheduled ticks, not a private clock. |
| Pistons | Movement preserves update ordering and streams its block changes. |
| Observers | A neighbour change schedules the one-shot observation response. |
| Powered and detector rails | Rail power follows the shared signal query. |
| Doors, trapdoors, and fence gates | Passive consumers update when their shared power input changes. |
| Dispensers and droppers | Inventory action is wired to redstone activation and the block-tick owner. |
| Hoppers | Transfers obey enabled state and the common tick budget. |
| Note blocks, tripwire, and targets | Each uses the common signal/notification path and exposes its visible effect. |

### World state

| feature | acceptance path |
|---|---|
| Time and daylight | The server advances and broadcasts time from the tick owner. |
| Weather | Rain and thunder state are server-authoritative and visible to clients. |
| Sleeping | A vote changes time and weather through their shared owners. |
| World border | The server enforces and publishes border state. |
| Game rules | Typed rules are stored, changed, broadcast, and read at their decision sites. |
| Difficulty | The server owns and applies difficulty rather than merely decoding it client-side. |
| Spawn and respawn points | Residency, joining, and respawning read the same world/player locations. |
| Dimensions and portal travel | A destination has a source, tick owner, storage, transfer path, and streamed view. |

### Operational services

| feature | acceptance path |
|---|---|
| Remote console | A listener executes against authoritative server state. |
| Query and status responses | A remote request receives state from the real host, not a client-side probe. |
| Resource-pack delivery | A server request reaches the connection state in which the client consumes it. |
| Plugin channels | Registered channels dispatch through the real connection path. |
| Access control | Operator, whitelist, and ban policy is enforced at connection admission. |
| Loot tables | Rolling feeds the same item/entity path players observe. |
| Advancements and statistics | Server-owned progress changes from real gameplay and reaches the client. |

## Island audit and corrections

The recurring audit question is “what consumes this?” Server protocol implementation, terrain
generation, decoded rule data, entity exposure, and time representations each require a production
consumer. Search the capability across workspace crates before declaring it absent: explosion exposure
can exist separately from block destruction, and a client-side time representation does not establish
server-side ownership. Keep these distinctions in feature reports and do not duplicate a neighbouring
subsystem merely because a narrow search missed it.

A capability is not connected until the chain is complete:

```
action or server event → authoritative state → tick/mutation owner
    → protocol directive → client state → pixels
```

The three original audit subjects remain mandatory end-to-end checks, with their current distinction
between closed wiring and residual feature work:

- **26.2 server protocol:** the host protocol is registered and the integrated server consumes it.
  The continuing gate is a real join that receives chunks and state updates; protocol crate tests alone
  are not sufficient.
- **Carvers and ore-feature placement:** generation data must be composed into the served chunk source,
  then verified from a streamed chunk rather than a generator-only test.
- **Game-rule values:** decoded rule values and server-side rule storage must meet at the decision
  sites that enforce them and at the broadcast path that makes them visible to a client.

Two corrections constrain future scope. Explosion work has separate entity-exposure/damage and
block-destruction halves; never implement the latter as if the former were absent. Time representation
also has separate client and server owners: a client time value does not advance, persist, or broadcast
server time. Search across crate boundaries before asserting either absence.

## Capacity and parallelism

Work inside a phase is parallel only when it does not share a chokepoint. The connection dispatcher,
tick loop, world state, chunk store, and protocol encoder are shared surfaces; keep primary feature
state in its own module and broker small wiring patches. Region persistence and ticket residency can
advance together, but unload/autosave wait for both. Redstone components are parallel only after the
shared propagation primitive is stable. Operational listeners and queries are independent leaves once
the host path exists.

Repository tracking capacity is not a design constraint. If work items need grouping, group by the
feature boundaries above; do not let tracker nesting determine server ownership.

## Verification rules

- Use reference-world files, captured independent-server bytes, or independent arithmetic. A
  `decode(encode(x))` loop does not prove compatibility.
- Prove absence detectors with a known negative control.
- Exercise save/reload, unload/reload, and tick-boundary behaviour separately.
- Measure pixels by known location and print a bounding box on failure.
- Test scheduled work both for its required execution and for non-occurrence when cancelled, blocked,
  or out of range.
- Run `cargo xtask connectedness` for a clientbound route, and trace serverbound work through its
  connection consumer.

## How to change it

Add simulation state near its authoritative owner, wire it through the production tick or mutation
choke immediately, and document its persistence and visible consumer. Add semantic operations to
`ServerProtocol`, implement them in the hosting family, and retain the boxed-protocol forward. Keep
native filesystem/network service policy at the boundary.

Detailed contracts live in [tick scheduling](../tick-scheduling.md),
[chunk storage](../chunk-storage.md), [world propagation](../world-propagation.md), and
[redstone](../redstone.md).

## Dependencies

This roadmap depends on `lodestone-server`, `lodestone-worldgen`, `lodestone-entity`,
`lodestone-net`, the version registry/protocol seam, and the shell as the visual consumer.
