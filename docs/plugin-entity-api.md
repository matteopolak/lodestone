# Plugin entity spawn/despawn/modify, and custom entity type registration

## What it is

Two independent halves, one per side of the client/server split, both giving a native plugin the
Bukkit-class `World.spawnEntity(loc, type)`/`Entity.remove()`/free-modification surface:

- **Server-side, real and cross-player-visible**: `IntegratedServer::spawn_mob`/`despawn_mob`
  (`crates/lodestone-server/src/integrated.rs`), backed by `MobSim::remove_mob`
  (`crates/lodestone-server/src/mobs/mod.rs`). A plugin embedding the server calls these directly —
  there is no dynamic plugin-loading mechanism yet, so "a native plugin" here means Rust code that
  depends on `lodestone-server` and holds an `IntegratedServer`, exactly the same relationship every
  other consumer of that crate already has.
- **Client-side, local-only**: `lodestone_ecs::entity_spawn` (`crates/lodestone-ecs/src/entity_spawn.rs`)
  — `spawn_entity`/`despawn_entity` for a **local, non-networked** entity, plus `CustomEntityRegistry`
  for a plugin's own logical entity kind that disguises as a real vanilla one for rendering. This is
  what a bevy-style client plugin (`crates/plugins/**`) reaches for; it never becomes visible to another
  real player, because that would need outbound wire injection, which `docs/plugin-api.md`'s
  packet-interception decision rules out permanently.

## How it works

### Server-side: reusing the accessor combat already shipped

The server's own `bevy_ecs::World` (`crate::ecs` in `lodestone-server`) is Phase 0 only — one counter
system, not threaded through `crate::tick::run_tick_loop` or `crate::mobs`'s actual simulation — so
there is no ECS-driven plugin surface to hang a spawn API off yet. The real, already-shipped surface
is simpler: `IntegratedServer::mobs() -> Option<&MobHandle>` hands out the same mutex-guarded handle
`crate::server::apply_attack` already mutates from a connection task, so a spawn/despawn needed no new
plumbing, only two missing primitives:

- `MobSim::spawn_species(entity_type, pos) -> &mut SimMob` and `SimMob::id(&self) -> i32` already
  existed — spawn-with-id was already almost free.
- `MobSim::remove_mob(id: i32) -> bool` did not exist at all. The only removal shape in the crate was
  two ad hoc `self.mobs.retain(|m| m.id != id)` call sites (a creeper self-detonation discard, and
  `reap_dead`'s death sweep), both bundled with death-only side effects (loot/XP) a plugin despawn must
  **not** trigger — Java's `Entity.remove()` drops nothing. `remove_mob` is that same retain shape,
  named and made public, with no side effects beyond the removal itself.

`IntegratedServer::spawn_mob`/`despawn_mob` are the thin plugin-facing wrappers: both `.map` over
`self.mobs()` and call `MobHandle::with` — `None` for a constructor with no tick loop, matching every
other `IntegratedServer` accessor's contract. Neither can touch a connected player: player entity ids
are allocated from `PLAYER_ENTITY_ID_BASE` and live in `PlayerRegistry`, never in `MobSim`'s own
`self.mobs`, so `despawn_mob` on a player's id is a harmless no-op — the server-side analogue of the
client's `apply_entity_removal` skipping an id held by `LocalPlayer`.

**"Modify" needed no new API here either.** A plugin already holds the exact `MobHandle` a live mob's
`SimMob` lives behind (`mobs.with(|sim| sim.get_mut(id))`), so healing, repositioning or re-equipping a
spawned mob is ordinary use of an accessor that already shipped for combat.

`crates/lodestone-server/tests/native_plugin_spawns_and_despawns_a_mob.rs` is the real-consumer gate: it
drives a real, running `IntegratedServer` through `spawn_mob`/`despawn_mob` only — never
`MobSim::spawn_species`/`remove_mob` directly, which would prove only that the underlying primitives
work, not that a plugin embedding the server can actually reach them — and reads the result back through
`IntegratedServer::mobs()`'s real handle. Three cases: spawn → modify → despawn → repeat-despawn is a
no-op; despawning an unrelated id removes nothing; both accessors answer `None` with no tick loop.

**Custom entity types, server-side, need no new registry.** `spawn_mob` already accepts any vanilla
`ResourceKey` as the disguise, so a plugin implementing a custom type server-side already can, today,
with its own plain `HashMap<ResourceKey, ResourceKey>` — there is no missing primitive, only the
absence of a *shared*, cross-plugin-validated registry (the value `CustomEntityRegistry` adds
client-side, covered below). That is a real but secondary gap: it matters once two independent server
plugins need to recognise each other's disguises, which nothing has asked for yet. Recovering "what did
I actually spawn" for a single plugin's own bookkeeping is exactly what
`lodestone-plugin-support::EntityDataStore` (a namespaced, entity-id-keyed key-value store, mirroring
Bukkit's `PersistentDataContainer`) already exists for.

### Client-side: why this reaches pixels with no render-side change

`lodestone_shell::entities::fold_entities` — the system that builds `lodestone-shell`'s render-side
entity track every frame — walks `lodestone_ecs::entity::EntityIndex` **generically**, by id, and
resolves each entry's draw facts by reading whatever components the entity happens to carry
(`resolve_entity_facts` requires `EntityKind`/`Position`/`Rotation`/`HeadYaw`; everything else is read
optionally). It does not ask how the entry got there. `crate::ingest::apply_entity_spawn` (a
wire-reported mob) and `entity_spawn::spawn_entity` (a plugin-spawned one) put exactly the same
component set on an entity indexed the same way, so a plugin-spawned entity is drawn the very next
`Extract` — no change to `lodestone-shell` at all. This is the property that keeps the feature from
being an island — a subsystem that is built and tested but reaches no pixels because nothing calls it.

"Modify" needed no new API on the client either: every component in `lodestone_ecs::entity` (`Position`,
`Rotation`, `Health`, `Equipment`, …) was already plugin-writable per `docs/plugin-api.md`'s "Reading and
writing state" table's non-player-entity-components row. Ordinary `Commands`/`Query` mutation is the
whole story there; `crates/plugins/lodestone-mob-spawner`'s own test exercises it directly (a plain
`Position`/`Health` insert) to confirm nothing about the new spawn/despawn path disturbs it.

### Client-side id safety

Vanilla's own entity-id counter (`Entity.ENTITY_COUNTER`) starts at `0` and only ever increments, so
every id a real server assigns — including the local player's own, via `apply_local_player_login` — is
non-negative. `PluginEntityIds` mints strictly negative ids (`-1`, `-2`, …), so a plugin-spawned
entity's id can never collide with a server-assigned one **by construction** — the two ranges do not
overlap, which is stronger than a runtime check that could miss a case. `is_plugin_entity_id` exposes
the test.

`despawn_entity` refuses to touch an id currently held by a `LocalPlayer` entity — the identical guard
`crate::ingest::apply_entity_removal` applies to a wire-reported removal, for the identical reason that
system's own doc comment records: nothing else stops a caller naming an id the local player happens to
hold, and despawning that entity would take `PhysicsState`, the HUD components and the driver's own
identity with it.

### Client-side custom entity types are a vanilla kind plus a tag, never a new registry id

The same wire ceiling `lodestone_game::custom_item` already solved for items: the wire protocol carries
an entity kind as a registry index into a fixed table, so a genuinely novel kind is not representable,
and a real Paper plugin solves it the same way — disguise the custom entity as a vanilla one.
`CustomEntityRegistry` is the entity-shaped mirror of `CustomItemRegistry`, with the same two namespace
rules (`CustomEntityRegistry::register`): the custom kind must not be `minecraft:`-namespaced (it would
collide with the real registry), and the disguise must be (a non-vanilla disguise cannot be rendered).
Registration is refused outright on a duplicate id, rather than silently replaced, for the same reason
`CustomItemRegistry::register` refuses one — two plugins claiming one id is a bug that must surface at
the registrant.

`spawn_custom_entity` resolves a registered custom kind to its disguise and spawns with `EntityKind`
carrying the **disguise** — never the logical kind — plus a new component, `CustomEntityKind`, carrying
the true logical kind for a plugin that wants to recover what it actually spawned. This is what keeps a
plugin-registered type out of `lodestone-render`'s model/texture corpus entirely: any lookup keyed off
`EntityKind` alone sees an ordinary, already-rigged vanilla entity and never has reason to ask whether a
plugin was involved, so the "no model, no texture" assertions the render-side corpus makes for an
*unmodeled* kind stay untouched — a plugin-registered type never reaches that path in the first place.
Asking for a kind nothing registered is a refusal (`UnknownCustomEntityType`), not a fallback disguise;
silently drawing a plugin's zombie disguise as some default mob because nobody registered it yet would
be a worse failure than a returned error.

### `PluginEntityIds` is installed by `CorePlugin`; `CustomEntityRegistry` is opt-in

`crate::CorePlugin` (`plugin.rs`, in `lodestone-ecs`) — the plugin every client `App` in the tree
installs — now also `init_resource`s `PluginEntityIds`. Spawning is basic enough, and a missing
resource behind a `ResMut<T>` system parameter panics at runtime ("Resource does not exist") with no
compile-time warning, that every `App` should have it the same way every `App` has
`WorldTime`/`FrameClock`. `CustomEntityRegistry` stays a separate, opt-in resource, following
`lodestone_ecs::items::CustomItemsPlugin`'s precedent exactly: `CustomEntityTypesExt::add_custom_entity_type`
installs it on first use, so a plugin that never registers a custom type never pays for the resource,
and one that does never has to remember a second `add_plugins` call. `EntitySpawnPlugin` exists for a
harness with no `IngestPlugin` in sight (idempotently installing `EntityIndex` and
`CustomEntityRegistry`) — every real client already gets `EntityIndex` from `IngestPlugin`
(`lodestone_app::client_app`'s default six plugins), so production code never needs it.

### `lodestone-mob-spawner`, the client-side real consumer

`MobSpawnerPlugin` installs `SpawnRequests`/`DespawnRequests`/`SpawnedEntities` (the same queued-request
shape `lodestone_worldedit::FillRequests` uses, for the same "needs synchronous drain-time application"
reason `docs/plugin-api.md` records for `ActionQueue`), registers one custom entity type
(`TRAINING_DUMMY`, disguised as `minecraft:zombie`) at build time, and drains both queues once per
`GameTick` — calling straight through to `entity_spawn`'s functions, never reimplementing id-minting or
the `LocalPlayer` guard.

`tests/drives_spawn_despawn_and_a_custom_type_through_the_schedule.rs` is the end-to-end gate, mirroring
`lodestone-worldedit`'s own real-schedule test: a real `App` with `CorePlugin` + `MobSpawnerPlugin`, a
queued request, a real `run_schedule(GameTick)` call, and every assertion read back through
`EntityIndex` — the same resource `fold_entities` walks — never by calling `entity_spawn`'s functions
directly. Three cases: a vanilla spawn, modify, and despawn round trip; the registered custom type
spawning with its `EntityKind` as the disguise and `CustomEntityKind` carrying the logical kind; and a
negative control confirming a despawn naming an untracked id (or the `LocalPlayer`'s own) is a harmless
no-op rather than a panic or a spurious removal.

## How to change it, and the gotchas

- **`MobSim::remove_mob` must never drop loot or grant experience.** That is `reap_dead`'s job on a real
  death; a plugin despawn is Java's plain `Entity.remove()`, and conflating the two would make every
  plugin-driven cleanup pass look like a kill.
- **`despawn_mob`/`despawn_entity` must never be able to remove a player**, on either side — the server
  guard is "player ids live in `PlayerRegistry`, never in `MobSim`'s `self.mobs`"; the client guard is
  the explicit `LocalPlayer` component check. Neither is a convention; both are structural (a player id
  is never in the collection being searched, or the check runs before any removal).
- **Never fall back to a default disguise for an unregistered custom kind.** `spawn_custom_entity`
  returns `UnknownCustomEntityType` instead — see "Custom entity types" above for why a silent
  wrong-mob render is worse than a returned error.
- **A custom kind's `EntityKind` must always carry the disguise, never the logical kind.** This is the
  entire reason the render-side model corpus's "no model, no texture" assertions stay correct for a
  plugin-registered type — breaking this invariant would hand a rig-less kind straight to a renderer
  that expects every `EntityKind` it sees to be a real, modeled vanilla one.
- **Do not fold `CustomEntityRegistry` into `CorePlugin`.** It follows `CustomItemsPlugin`'s opt-in
  precedent deliberately.
- **A shared, cross-plugin server-side custom-type registry is a real but secondary gap**, not a missing
  primitive — see "Custom entity types, server-side, need no new registry" above. Build one the way
  `CustomEntityRegistry` is built (two namespace rules, refuse-on-duplicate) if and when two independent
  server plugins actually need to recognise each other's disguises.
- **Re-check `lodestone-server`'s `crate::ecs` module doc before assuming the server-ECS migration has
  landed further than this file records.** As of this writing it states plainly that it is
  "deliberately shallow" and moves no state; `spawn_mob`/`despawn_mob` deliberately do not depend on it.

## Configuration

None. A client plugin adds `lodestone_ecs::CorePlugin` (for `PluginEntityIds`) plus its own plugin,
exactly as every other plugin in `crates/plugins/` does. A server-side consumer needs nothing beyond a
running `IntegratedServer` built with a tick loop (any `open_in_memory_with_mobs`/`open_persistent_with_mobs`
constructor).

## Dependencies

`lodestone_ecs::entity_spawn` depends on `crate::entity` (the component set and `EntityIndex`) and
`lodestone_model` (`ResourceKey`/`Vec3`/`Rotation`); no protocol crate, since a plugin-spawned entity
never names a numeric id. `lodestone-mob-spawner` depends on `lodestone-ecs` and `lodestone-model` only.
`IntegratedServer::spawn_mob`/`despawn_mob` add no new dependency to `lodestone-server` — both are built
entirely on `crate::mobs::MobHandle`, already a dependency of that same file.

## See also

- [`plugin-api.md`](plugin-api.md) — the intent doctrine, the components a plugin can already read and
  write, and the packet-interception decision ruling out server-visible outbound injection.
- [`packet-wiring.md`](packet-wiring.md) — why a disguise visible to *other* players needs outbound byte
  mutation and stays out of reach, versus what `ActionVetoes`/`EgressFilters` already serve.
