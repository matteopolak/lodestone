# Plugin data, config, and durable records

## What it is

`lodestone-plugin-support` is the shared native-plugin convenience crate. It provides a confined
per-plugin directory, typed JSON config, live entity/chunk metadata, and bounded durable records for
plugin, world, player, and generation-qualified entity scopes.

## How it works

`PluginDataStore` keeps validated `PluginDataKey` values in deterministic key order. Each value is an
opaque byte blob with a non-zero plugin schema version; typed `set`/`get` methods use JSON only at the
plugin boundary. Namespaces and keys are path-safe, blobs are capped at 1 MiB, restores are capped at
16,384 records, and a JSON snapshot is capped at 16 MiB. An entity key includes both its UUID bytes and
its lifecycle generation, so reusing a runtime identity cannot expose the previous occupant's data.

World owners can call `snapshot` or `snapshot_scope` before a save transaction and
`restore_entries` after opening a world. `unload_scope` removes only resident memory and returns the
owned entries; it is not deletion from durable storage. `migrate` permits monotonic schema upgrades
and rejects downgrades. The file adapter writes an exclusive, synced temporary sibling, renames it,
and syncs the parent directory. Missing files open as empty; malformed existing files are errors.

The conventional `lodestone-plugin-data.json` sidecar is independent of terrain encoding. An Anvil
world and a Lodestone-native world can therefore use the same record envelope beside their respective
stores without teaching either terrain codec to decode plugin-owned bytes. The WASM host's confined
filesystem uses the same bounded atomic-write and delete lifecycle; a typed snapshot can be carried
through that path unchanged.

## How to change it

Add fields to `DataScope` only when the identity is stable across a world reopen. Keep runtime IDs out
of durable keys unless a generation is included. New limits or envelope fields require a format-version
decision and tests for old, malformed, oversized, duplicate, and future-version records. Keep the file
adapter storage-neutral: server backends choose when to snapshot and commit, while this crate validates
and owns the plugin bytes.

The live `EntityDataStore` and `ChunkDataStore` are intentionally separate ECS resources. They do not
evict automatically on despawn; callers must remove live entries, or use generation-qualified durable
records for restart-safe state.

## Configuration

`MAX_PLUGIN_DATA_BLOB_BYTES`, `MAX_PLUGIN_DATA_RECORDS`, and `MAX_PLUGIN_DATA_SNAPSHOT_BYTES` are the
durable-record limits. `PLUGIN_DATA_SNAPSHOT_FILE` and `plugin_data_snapshot_path` define the shared
world sidecar name. The native config helper uses the platform data directory selected by
`lodestone-auth`; the WASM host requires an embedding-provided filesystem root and a granted filesystem
capability.

## Dependencies

The crate uses `serde`/`serde_json` for the typed boundary, `lodestone-auth` for native plugin paths,
and the public ECS/world crates for live metadata resources. The optional file adapter uses the standard
filesystem only; it does not depend on Anvil, Lodestone storage, or a server implementation.
