# Plugin entity spawn/despawn/modify, and custom entity type registration

## What it is

`lodestone_ecs::entity_spawn` (`crates/lodestone-ecs/src/entity_spawn.rs`) — a plugin-facing
`spawn_entity`/`despawn_entity` pair for creating and destroying a **local, non-networked** entity, plus
`CustomEntityRegistry` for registering a logical entity kind of a plugin's own that disguises as a real
vanilla one for rendering. This is the achievable half, today, of the Bukkit-class
`World.spawnEntity`/`Entity.remove()` API and custom entity-type registration:
`docs/plugin-api.md`'s "packet-interception" section already settled that outbound wire mutation (the
route a *server*-authoritative, other-players-see-it spawn would need) is permanently out of reach for a
plugin, and `lodestone-server`'s own `bevy_ecs::World` (`crate::ecs` in that crate) is only Phase 0 of its
migration — it runs one counter system and is not yet threaded through `crate::tick::run_tick_loop` or
`crate::mobs`'s actual mob simulation, so there is no real server-side entity simulation to hang a plugin
API off yet. A client-only fake entity — visible locally, drawn through the exact same render path a
wire-reported mob uses — is the part that is real and load-bearing today.

`crates/plugins/lodestone-mob-spawner` is the real consumer: a small plugin that queues spawn/despawn
requests and drains them once per `GameTick`, demonstrating the Bukkit archetype named in the motivating
issue — "the basis of every minigame, mob-farm plugin and disguise plugin."

## How it works

### Why this reaches pixels with no render-side change

`lodestone_shell::entities::fold_entities` — the system that builds `lodestone-shell`'s render-side
entity track every frame — walks `lodestone_ecs::entity::EntityIndex` **generically**, by id, and resolves
each entry's draw facts by reading whatever components the entity happens to carry
(`resolve_entity_facts` requires `EntityKind`/`Position`/`Rotation`/`HeadYaw`; everything else is read
optionally). It does not ask how the entry got there. `crate::ingest::apply_entity_spawn` (a wire-reported
mob) and `entity_spawn::spawn_entity` (a plugin-spawned one) put exactly the same component set on an
entity indexed the same way, so a plugin-spawned entity is drawn the very next `Extract` — no change to
`lodestone-shell` at all. This is the property that keeps the feature from being an island — a subsystem
that is built and tested but reaches no pixels because nothing calls it.

"Modify" needed no new API: every component in `lodestone_ecs::entity` (`Position`, `Rotation`, `Health`,
`Equipment`, …) was already plugin-writable per `docs/plugin-api.md`'s "Reading and writing state" table's
non-player-entity-components row. Ordinary `Commands`/`Query` mutation is the whole story there;
`crates/plugins/lodestone-mob-spawner`'s own test exercises it directly (a plain `Position`/`Health`
insert) to confirm nothing about the new spawn/despawn path disturbs it.

### Id safety

Vanilla's own entity-id counter (`Entity.ENTITY_COUNTER`) starts at `0` and only ever increments, so every
id a real server assigns — including the local player's own, via `apply_local_player_login` — is
non-negative. `PluginEntityIds` mints strictly negative ids (`-1`, `-2`, …), so a plugin-spawned entity's
id can never collide with a server-assigned one **by construction** — the two ranges do not overlap,
which is stronger than a runtime check that could miss a case. `is_plugin_entity_id` exposes the test.

`despawn_entity` refuses to touch an id currently held by a `LocalPlayer` entity — the identical guard
`crate::ingest::apply_entity_removal` applies to a wire-reported removal, for the identical reason that
system's own doc comment records: nothing else stops a caller naming an id the local player happens to
hold, and despawning that entity would take `PhysicsState`, the HUD components and the driver's own
identity with it.

### Custom entity types are a vanilla kind plus a tag, never a new registry id

The same wire ceiling `lodestone_game::custom_item` already solved for items: the wire protocol carries an
entity kind as a registry index into a fixed table, so a genuinely novel kind is not representable, and a
real Paper plugin solves it the same way — disguise the custom entity as a vanilla one. `CustomEntityRegistry`
is the entity-shaped mirror of `CustomItemRegistry`, with the same two namespace rules
(`CustomEntityRegistry::register`): the custom kind must not be `minecraft:`-namespaced (it would collide
with the real registry), and the disguise must be (a non-vanilla disguise cannot be rendered). Registration
is refused outright on a duplicate id, rather than silently replaced, for the same reason
`CustomItemRegistry::register` refuses one — two plugins claiming one id is a bug that must surface at the
registrant.

`spawn_custom_entity` resolves a registered custom kind to its disguise and spawns with `EntityKind`
carrying the **disguise** — never the logical kind — plus a new component, `CustomEntityKind`, carrying the
true logical kind for a plugin that wants to recover what it actually spawned. This is what keeps a
plugin-registered type out of `lodestone-render`'s model/texture corpus entirely: any lookup keyed off
`EntityKind` alone sees an ordinary, already-rigged vanilla entity and never has reason to ask whether a
plugin was involved, so the "no model, no texture" assertions the render-side corpus makes for an
*unmodeled* kind stay untouched — a plugin-registered type never reaches that path in the first place.
Asking for a kind nothing registered is a refusal (`UnknownCustomEntityType`), not a fallback disguise;
silently drawing a plugin's zombie disguise as some default mob because nobody registered it yet would be
a worse failure than a returned error.

### `PluginEntityIds` is installed by `CorePlugin`; `CustomEntityRegistry` is opt-in

`crate::CorePlugin` (`plugin.rs`) — the plugin every `App` in the tree installs — now also
`init_resource`s `PluginEntityIds`. Spawning is basic enough, and a missing resource behind a `ResMut<T>`
system parameter panics at runtime ("Resource does not exist") with no compile-time warning, that every
`App` should have it the same way every `App` has `WorldTime`/`FrameClock`. `CustomEntityRegistry` stays a separate, opt-in resource,
following `lodestone_ecs::items::CustomItemsPlugin`'s precedent exactly: `CustomEntityTypesExt::add_custom_entity_type`
installs it on first use, so a plugin that never registers a custom type never pays for the resource, and
one that does never has to remember a second `add_plugins` call. `EntitySpawnPlugin` exists for a harness
with no `IngestPlugin` in sight (idempotently installing `EntityIndex` and `CustomEntityRegistry`) — every
real client already gets `EntityIndex` from `IngestPlugin` (`lodestone_app::client_app`'s default six
plugins), so production code never needs it.

### `lodestone-mob-spawner`, the real consumer

`MobSpawnerPlugin` installs `SpawnRequests`/`DespawnRequests`/`SpawnedEntities` (the same queued-request
shape `lodestone_worldedit::FillRequests` uses, for the same "needs synchronous drain-time application"
reason `docs/plugin-api.md` records for `ActionQueue`), registers one custom entity type
(`TRAINING_DUMMY`, disguised as `minecraft:zombie`) at build time, and drains both queues once per
`GameTick` — calling straight through to `entity_spawn`'s functions, never reimplementing id-minting or the
`LocalPlayer` guard.

`tests/drives_spawn_despawn_and_a_custom_type_through_the_schedule.rs` is the end-to-end gate, mirroring
`lodestone-worldedit`'s own real-schedule test: a real `App` with `CorePlugin` + `MobSpawnerPlugin`, a
queued request, a real `run_schedule(GameTick)` call, and every assertion read back through `EntityIndex` —
the same resource `fold_entities` walks — never by calling `entity_spawn`'s functions directly. Three
cases: a vanilla spawn, modify, and despawn round trip; the registered custom type spawning with its
`EntityKind` as the disguise and `CustomEntityKind` carrying the logical kind; and a negative control
confirming a despawn naming an untracked id (or the `LocalPlayer`'s own) is a harmless no-op rather than a
panic or a spurious removal.

## How to change it, and the gotchas

- **Never fall back to a default disguise for an unregistered custom kind.** `spawn_custom_entity` returns
  `UnknownCustomEntityType` instead — see "Custom entity types" above for why a silent wrong-mob render is
  worse than a returned error.
- **A custom kind's `EntityKind` must always carry the disguise, never the logical kind.** This is the
  entire reason the render-side model corpus's "no model, no texture" assertions stay correct for a
  plugin-registered type — breaking this invariant would hand a rig-less kind straight to a renderer that
  expects every `EntityKind` it sees to be a real, modeled vanilla one.
- **Do not fold `CustomEntityRegistry` into `CorePlugin`.** It follows `CustomItemsPlugin`'s opt-in
  precedent deliberately — see "PluginEntityIds is installed by CorePlugin" above.
- **A server-side, cross-player-visible version of this is blocked on real architecture, not on this
  module.** Threading `&mut World` through `lodestone-server`'s `crate::tick::run_tick_loop`, and later
  moving `crate::mobs`'s actual simulation onto that `World`, are the prerequisite for a spawn a *second*
  real player would see; `docs/plugin-api.md`'s packet-interception decision permanently forecloses the
  alternative (outbound packet injection from every plugin). Re-check `lodestone-server`'s `crate::ecs`
  module doc for its own current phase before assuming it has landed further than this file records — as
  of this writing it states plainly that it is "deliberately shallow" and moves no state.

## Configuration

None. A plugin adds `lodestone_ecs::CorePlugin` (for `PluginEntityIds`) plus its own plugin, exactly as
every other plugin in `crates/plugins/` does.

## Dependencies

`lodestone_ecs::entity_spawn` depends on `crate::entity` (the component set and `EntityIndex`) and
`lodestone_model` (`ResourceKey`/`Vec3`/`Rotation`); no protocol crate, since a plugin-spawned entity never
names a numeric id. `lodestone-mob-spawner` depends on `lodestone-ecs` and `lodestone-model` only.

## See also

- [`plugin-api.md`](plugin-api.md) — the intent doctrine, the components a plugin can already read and
  write, and the packet-interception decision ruling out server-visible outbound injection.
- [`packet-wiring.md`](packet-wiring.md) — why a disguise visible to *other* players needs outbound byte
  mutation and stays out of reach, versus what `ActionVetoes`/`EgressFilters` already serve.
