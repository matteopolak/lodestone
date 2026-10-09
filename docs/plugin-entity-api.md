# Plugin entity spawn/despawn/modify, and custom entity types

## What it is

The Bukkit-class `spawnEntity` / `remove` / modify surface for native plugins, in two independent halves. Server-side, real and visible to every player: `IntegratedServer::spawn_mob`/`despawn_mob` (`crates/lodestone-server/src/integrated.rs`) backed by `MobSim::remove_mob`, plus `IntegratedServer::entity_api`. Client-side, local only: `lodestone_ecs::entity_spawn` with `CustomEntityRegistry`, never visible to other players (that would need outbound wire injection, which [plugin API](plugin-api.md) rules out).

## How it works

### Server side

There is no dynamic plugin loading here: "a native plugin" is Rust code depending on `lodestone-server` and holding an `IntegratedServer`. The server's `bevy_ecs::World` (`crate::ecs`) is deliberately shallow and carries no mob state, so the surface sits on `IntegratedServer::mobs() -> Option<&MobHandle>`, the mutex-guarded handle combat already mutates.

- `MobSim::spawn_species(entity_type, pos) -> &mut SimMob` and `SimMob::id` already existed.
- `MobSim::remove_mob(id) -> bool` is the named, public form of the retain shape used by creeper detonation and `reap_dead`, without death-only side effects. A plugin despawn drops nothing and grants no XP.
- `spawn_mob`/`despawn_mob` map over `self.mobs()` and return `None` with no tick loop. Player ids come from `PLAYER_ENTITY_ID_BASE` and live in `PlayerRegistry`, never in `MobSim`'s mob list, so despawning a player id is a structural no-op.
- Modify needs no new API: `mobs.with(|sim| sim.get_mut(id))`.

`ServerEntityApi` is the typed request boundary that never exposes a lock guard. References are `EntityNetworkId`; raw ids are classified with `EntityNetworkId::from_wire`. `observe` returns an owned identity, motion, health and six-slot equipment copy (empty slots explicit; mob equipment is retained beside the sim record at spawn, so observers see what combat and ranged goals use). `mutate` takes typed knockback, health, effect, teleport and despawn operations: mob knockback changes the streamed snapshot, player teleports and effects go through `PlayerRegistry`'s directed queue so the owning connection emits the packet. Unsupported operations and unknown ids are reported, never applied elsewhere.

`EntityLifecycleCursor` is the polling lifecycle: the plugin owns the cursor, the first poll reports the current population, later polls report additions and removals (a removal carries the last copied observation). No event log, callback, ECS handle or guard is retained; updates to survivors need `observe`.

Server-side custom types need no registry: `spawn_mob` accepts any vanilla `ResourceKey` as the disguise, and a plugin can keep its own map, with `lodestone-plugin-support::EntityDataStore` (a namespaced entity-id key-value store mirroring Bukkit's `PersistentDataContainer`) for "what did I spawn". A shared cross-plugin registry is a secondary gap.

The gate `crates/lodestone-server/tests/native_plugin_spawns_and_despawns_a_mob.rs` drives a running `IntegratedServer` through `spawn_mob`/`despawn_mob` only and reads back via `mobs()`.

### Client side

`lodestone_shell::entities::fold_entities` walks `EntityIndex` generically and reads components (`resolve_entity_facts` needs `EntityKind`, `Position`, `Rotation`, `HeadYaw`; the rest are optional). `ingest::apply_entity_spawn` and `entity_spawn::spawn_entity` produce the same component set, so a plugin-spawned entity draws on the next `Extract` with no shell change. Modifying uses ordinary `Commands`/`Query` writes on the `lodestone_ecs::entity` components. `lodestone-mob-spawner::EntityMutationRequests` (`Teleport`, `SetVelocity`, `SetHealth`, `SetEquipment`) is a request-boundary example resolved during `GameTick` through deferred commands; unknown ids are no-ops. It is client-local; server-visible mutation goes through the authoritative mob handle.

**Id safety.** Server ids are non-negative (including the local player's), and `PluginEntityIds` mints strictly negative ids (`-1`, `-2`, ...), so collisions are impossible by construction; `is_plugin_entity_id` tests it. `despawn_entity` refuses an id held by a `LocalPlayer` (the same guard as `apply_entity_removal`, since removing it would take `PhysicsState`, the HUD and driver identity). `EntityNetworkId::{Server, Plugin}` makes ownership explicit through `EntityIndex::get_typed`/`insert_typed`/`remove_typed`; `despawn_entity` rejects non-negative values before consulting the index.

**Custom types.** The wire carries entity kind as an index into a fixed table, so a plugin disguises a custom kind as a vanilla one, as Paper plugins do. `CustomEntityRegistry::register` requires the custom kind not be `minecraft:` and the disguise be, and refuses duplicates. `spawn_custom_entity` spawns with `EntityKind` set to the disguise and a `CustomEntityKind` component holding the logical kind; renderer lookups keyed on `EntityKind` see an ordinary modelled entity, so the render corpus's unmodelled-kind assertions are untouched. An unregistered kind returns `UnknownCustomEntityType`, never a default mob.

**Installation.** `CorePlugin` (`lodestone-ecs`, installed by every client `App`) inits `PluginEntityIds`, since a missing `ResMut` resource panics at runtime. `CustomEntityRegistry` is opt-in via `CustomEntityTypesExt::add_custom_entity_type` (the `CustomItemsPlugin` precedent). `EntitySpawnPlugin` exists for harnesses without `IngestPlugin`.

**`lodestone-mob-spawner`.** `MobSpawnerPlugin` installs `SpawnRequests`/`DespawnRequests`/`SpawnedEntities`, registers `TRAINING_DUMMY` (disguised as `minecraft:zombie`), and drains both queues per `GameTick` through `entity_spawn`'s functions. Its end-to-end test runs a real `App` schedule and reads back through `EntityIndex`, with a negative control for untracked and `LocalPlayer` ids.

## How to change it

- `remove_mob` must never drop loot or grant XP; that is `reap_dead`'s job.
- Despawn must never remove a player on either side (structural: the id is not in the searched collection, or the `LocalPlayer` check runs first).
- Never default-disguise an unregistered custom kind.
- A custom kind's `EntityKind` must carry the disguise, or a rig-less kind reaches a renderer that assumes modelled kinds.
- Keep `CustomEntityRegistry` out of `CorePlugin`.
- A shared server-side registry, if two server plugins ever need one, should copy `CustomEntityRegistry`'s two namespace rules and refuse-on-duplicate.
- Check the `crate::ecs` module doc before assuming the server-ECS migration has progressed; `spawn_mob`/`despawn_mob` deliberately do not depend on it.

## Configuration

None. A client plugin adds `CorePlugin` plus its own plugin; a server consumer needs an `IntegratedServer` with a tick loop (`open_in_memory_with_mobs` / `open_persistent_with_mobs`).

## Dependencies

`lodestone_ecs::entity_spawn` uses `crate::entity` and `lodestone_model` (`ResourceKey`, `Vec3`, `Rotation`), no protocol crate. `lodestone-mob-spawner` uses `lodestone-ecs` and `lodestone-model`. `spawn_mob`/`despawn_mob` add no dependency beyond `crate::mobs::MobHandle`. See also [plugin API](plugin-api.md) and [packet wiring](packet-wiring.md) (why a disguise visible to other players needs outbound byte mutation and stays out of reach).
