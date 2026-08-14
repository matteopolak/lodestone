# Plugin data directory, config, and persistent key-value storage

## What it is

`crates/plugins/lodestone-plugin-support` — shared, non-engine conveniences every native plugin would
otherwise reimplement for itself: a per-plugin data directory and typed config file (mirroring
`JavaPlugin.getDataFolder()`/`getConfig()`), and an in-memory, namespaced key-value store attachable to
an entity or a chunk (the non-persistent half of a persistent data container, mirroring Bukkit's
`PersistentDataContainer`/`Metadatable.setMetadata`). Neither is engine surface — deleting this crate leaves a working client, per
`crates/plugins/README.md`'s own test for what belongs under `crates/plugins/`.

## How it works

### Data directory and config (`src/paths.rs`, `src/config.rs`)

`lodestone_plugin_support::paths::plugin_data_dir(name)` is `lodestone_auth::paths::data_dir().join("plugins").join(name)`
— the one platform-data-directory implementation this codebase already settled on, with a `plugins/<name>`
layer added on top. `ensure_plugin_data_dir` additionally creates it. `config::load_or_default`/`config::save`
are the typed helpers on top: JSON (via `serde_json`, already a workspace dependency), forgiving on read
(a missing or corrupt file loads as `T::default()`, matching `JavaPlugin.getConfig()`'s own "no config
file yet" behaviour rather than surfacing a load error every plugin author has to handle).

```rust,ignore
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct MyConfig { greeting: String }

impl Plugin for MyPlugin {
    fn build(&self, app: &mut App) {
        let cfg: MyConfig = lodestone_plugin_support::config::load_or_default("my-plugin", "config.json");
        app.insert_resource(MyConfigResource(cfg));
    }
}
```

`tests/a_real_plugin_persists_its_config.rs` is the real consumer: a toy `Plugin::build` loads its config
the same way, exercised through a real `bevy_ecs::app::App`, not a bare function call — one test asserts
a fresh install boots with defaults, the other saves a config as if from "a previous run" and asserts a
fresh `App` loads exactly that.

### Persistent data container (`src/persistent_data.rs`)

Two resources, `EntityDataStore` (keyed by `lodestone_ecs::entity::MinecraftEntityId`) and `ChunkDataStore`
(keyed by `lodestone_world::ChunkPos`), each a `HashMap` of `"<plugin>:<key>"` → `serde_json::Value`.
`namespaced_key(plugin, key)` builds the key so two plugins choosing the same bare name (`"balance"`,
`"level"`) do not collide. `PersistentDataPlugin` installs both resources, `is_unique() == false` so more
than one plugin adding it is a no-op rather than a panic — matching `lodestone_shop_api::ShopApiPlugin`'s
own reasoning.

`tests/drives_through_a_real_schedule.rs` is the real consumer, and the strongest form available: a real
`App` with `lodestone_ecs::CorePlugin` + `PersistentDataPlugin` + a toy plugin whose system runs every
`GameTick`, incrementing a per-entity and a per-chunk counter through `Query`/`ResMut`. The negative
control (`an_entity_spawned_partway_through_only_accumulates_its_own_ticks`) is what actually proves the
system runs every tick rather than once: an entity spawned after three ticks and ticked four more reads
back exactly `4`, which a system that silently never re-ran would fail.

### Why `serde_json::Value`, not a decoded struct — the NBT hazard this avoids

`CLAUDE.md` names a specific, paid-for hazard: a round-trip that decides which fields to carry through by
consulting a **static name list** silently drops data it failed to decode (the `Age`/`Health` NBT-type-
collision incident). That hazard is about excluding a field *because* a schema does not name it. This
store never decodes into named fields at all — every value is an opaque `Value` keyed by whatever string
the plugin chose, so there is no schema to fall out of sync with. **This matters again the moment
persistence is added**: whoever builds the Tier 2 (survives-a-restart) half of the persistent data container must carry each
entry through wholesale — one opaque NBT compound blob per key — rather than decoding into a fixed set of
named fields and excluding whatever a schema does not list, which is exactly the shape that hazard needs
to recur.

### The id-reuse hazard, named rather than solved here

`docs/entity-components.md` records that a despawned entity's id can be reused, and that
`apply_entity_removal` specifically guards against taking out the local player's identity for that reason.
`EntityDataStore` has **no automatic eviction** on despawn: wiring that would mean reading `lodestone-ecs`'s
own ingest fold, outside this crate's dependency direction (plugins depend on the engine, never the
reverse). A plugin that cares must call `EntityDataStore::remove_entity` itself when it observes a despawn
(e.g. via `lodestone_ecs::GameEvent`), or accept that a reused id could read back a previous occupant's
data.

## How to change it, and the gotchas

- **Do not add automatic entity-despawn eviction inside this crate.** It would need a dependency on
  `lodestone-ecs`'s ingest fold or the shell's driver, which inverts the plugin → engine dependency
  direction `crates/plugins/README.md` enforces (`cargo xtask check-isolation`/`check-connected`). If this
  is ever wanted by default, it belongs as a system the *engine* registers, not this crate.
- **Keep values opaque (`serde_json::Value`), never decode into named fields at the storage layer.** See
  the NBT hazard note above — this is deliberate, not an oversight to "improve" by adding a typed schema.
- **`paths`/`config` avoid `std::env::set_var` in tests entirely**, mirroring `lodestone_auth::paths`'s own
  split: a pure `_under`/`_from` function takes the base directory as a parameter, so tests never mutate
  process-wide environment state (which `deny(unsafe_code)` also makes require an `unsafe` block, since
  `std::env::set_var` is `unsafe` as of the edition this workspace targets).

## Configuration

None beyond the `plugin_name`/`file_name` a plugin author chooses when calling into `paths`/`config`.
`LODESTONE_DATA_DIR` overrides the base directory (inherited from `lodestone_auth::paths::data_dir`), same
as every other on-disk state in this codebase.

## Dependencies

`lodestone-auth` (`paths::data_dir`), `lodestone-ecs` (`entity::MinecraftEntityId`, the plugin ABI),
`lodestone-world` (`ChunkPos`), `serde`/`serde_json`, `bevy_ecs`/`bevy_app` (direct, for the derive
macros' absolute paths — see `docs/plugin-api.md`'s "how to change it").

## See also

- [`docs/plugin-api.md`](./plugin-api.md) — the plugin ABI and doctrine this crate sits on top of.
- [`docs/worldedit-plugin.md`](./worldedit-plugin.md) — the sibling plugin built on the same crate
  ownership cluster, for the bulk-edit region API.
- [`docs/entity-components.md`](./entity-components.md) — the id-reuse hazard this doc's persistent-data
  section names but does not solve.
