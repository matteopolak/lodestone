# Server simulation — the roadmap

## What it is

The ordering, dependency edges and acceptance paths for server-side world simulation: chunk lifecycle, persistence, block behaviour, redstone, world state, the tick loop and operational plumbing. Command execution is a separate subsystem; mob AI, pathfinding, breeding, villagers and raids are in [`server-entities.md`](./server-entities.md).

## How it works

Foundations already in place (reuse these owners, never recreate them): independent reference gates for worldgen, collision shapes, hardness, entity dimensions and block physics; a transport-neutral connection loop with in-memory and TCP transports; the registered 26.2 server protocol; an NBT reader/writer; and a 20 Hz tick loop with MSPT/TPS accounting.

```
tick and protocol core
  ├── chunk residency ────────────────┐
  ├── persistence ◀───────────────────┘  (unload hands state to storage)
  ├── scheduled/block ticks ──→ redstone components
  ├── world state ──→ sleep, spawn location, dimension transfer
  └── operational services
```

Order: server core first; chunk residency and persistence together, but enable unload/autosave only once both handoffs exist; scheduled ticks and neighbour propagation before any redstone component; then world-state features (sleep after time/weather/rules, spawn residency after world spawn). Remote console, query, status, resource-pack, plugin-channel and access work are parallel leaves once the host path exists. Multi-dimension work is a combined generation, storage, tick, transfer and stream feature, not a small portal patch.

### Work inventory (feature: acceptance path)

- **Server core:** unified 20 Hz tick loop publishing observable time/mob/block-entity/effect updates; MSPT/TPS accounting from the tick owner; shell singleplayer protocol wiring as one join-to-render path.
- **Chunk lifecycle:** tickets and loading priority through the full empty-to-full pipeline; view and simulation distance following player movement; unload with save-on-unload and reload; asynchronous generation that never blocks the connection loop; served carvers and ore features visible in a streamed chunk; spawn-area residency.
- **Persistence:** region storage surviving independent read and reload; world metadata through the tick loop's owner; player data returning through login; per-chunk entity and POI storage; autosave coordinated with tick ownership and data-version handling.
- **Block behaviour:** random ticks; scheduled ticks and bounded neighbour propagation; fluids streaming to clients; crops, saplings, leaves; gravity blocks; fire; explosion block destruction joined to entity exposure.
- **Redstone:** dust and torches, repeaters and comparators (scheduled ticks, not a private clock), pistons, observers, rails, doors/trapdoors/gates, dispensers/droppers, hoppers, note blocks/tripwire/targets, all through the shared signal and notification path.
- **World state:** server-owned time, weather, sleeping, world border, typed game rules (stored, changed, broadcast, read at decision sites), difficulty, spawn and respawn points, dimensions and portal travel (source, tick owner, storage, transfer path, streamed view).
- **Operational services:** remote console, query and status from the real host, resource-pack delivery, plugin channels, access control at admission, loot tables, advancements and statistics.

### Island audit

Ask "what consumes this?". A capability is connected only when the chain is complete: action or server event, authoritative state, tick/mutation owner, protocol directive, client state, pixels. Search across crate boundaries before declaring a capability absent.

- **Server protocol:** the continuing gate is a real join receiving chunks and state updates, not protocol crate tests.
- **Carvers and ores:** compose generation data into the served chunk source and verify from a streamed chunk.
- **Game rules:** decoded values and server storage must meet at enforcing decision sites and the broadcast path.
- **Explosions** have separate entity-exposure/damage and block-destruction halves; **time** has separate client and server owners (a client time value does not advance, persist or broadcast server time).

### Parallelism and verification

The connection dispatcher, tick loop, world state, chunk store and protocol encoder are shared chokepoints: keep feature state in its own module and broker small wiring patches. Redstone components parallelise only once propagation is stable. Group work by the feature boundaries above, not tracker nesting.

- Use reference-world files, captured independent-server bytes or independent arithmetic, never `decode(encode(x))`.
- Prove absence detectors with a negative control; exercise save/reload, unload/reload and tick boundaries separately; measure pixels by location with a bounding box on failure.
- Test scheduled work for execution and for non-occurrence when cancelled, blocked or out of range.
- Run `cargo xtask connectedness` for a clientbound route; trace serverbound work through its connection consumer.

## How to change it

Add simulation state near its authoritative owner, wire it through the production tick or mutation choke immediately, and document its persistence and visible consumer. Add semantic operations to `ServerProtocol`, implement them in the hosting family, retain the boxed-protocol forward, and keep native filesystem/network policy at the boundary. Contracts: [tick scheduling](../tick-scheduling.md), [chunk storage](../chunk-storage.md), [world propagation](../world-propagation.md), [redstone](../redstone.md).

## Dependencies

`lodestone-server`, `lodestone-worldgen`, `lodestone-entity`, `lodestone-net`, the version registry/protocol seam, and the shell as the visual consumer.
