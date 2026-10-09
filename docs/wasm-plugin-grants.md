# WASM plugin grants

## What it is

`--plugin-grants <FILE>` is the shell's opt-in, persisted configuration for granting capabilities beyond the WASM host's fail-closed baseline. A grant applies to one discovered plugin instance, identified by both its discovery-root-relative `plugin.toml` path and that manifest's `name`.

## How it works

- The shell reads the JSON once, just before `lodestone::wasm_plugins::install_from_directory_with_grants` discovers `plugins/`. Each entry becomes a `lodestone_wasm_host::PluginIdentity` plus a `CapabilitySet`, matched on both fields per manifest. An unmatched plugin keeps `CapabilitySet::default_policy`.

  ```json
  {"grants": [{"manifest_path": "trusted/plugin.toml", "name": "trusted",
               "capabilities": ["fs:read", "act:place", "observe:place"]}]}
  ```

- `manifest_path` is relative to the discovery root, ends in `plugin.toml`, and has no `.` or `..` segment. The parser rejects unknown fields, duplicate `(manifest_path, name)` pairs, unreadable or malformed files and any name `lodestone_wasm_host::Capability::parse` does not know; it never drops a request or applies a partial config.
- The policy is not watched. The desktop launcher reads it at startup; a native embedding can call `lodestone::wasm_plugins::reload_from_directory_with_grant_file`, which parses the whole file before staging, re-reads every manifest and module against it, and rejects the entire replacement (leaving active guests untouched) on a malformed policy, denied capability, bad manifest, invalid module, command collision or graph error. So a changed file never changes a running plugin's authority without a revalidated reload. At commit, old stores drop in reverse dependency/load order, staged stores take their place in priority/name order, the conductor removes only command roots it installed before registering the new ones (never a native plugin's command), guest-owned pending intents are discarded and outcome cursors start empty.
- `plugin.toml` may declare `[dependencies]` with `required` and `optional` name lists. Required ones must exist in the same discovery directory; optional ones are ordering edges when present. Discovery sorts topologically with priority, manifest name then path as tie-breakers, and reports missing dependencies, duplicate names and cycles deterministically. Startup can report a bad entry while loading independent valid ones.
- `version:broker` is a separate privileged opt-in (withheld by default). A manifest requesting it must declare `[version-lock]` with exact `family`, `protocol` and broker `abi`; the host compares all three against its `VersionBroker` before reading the module. The WIT import returns only copied descriptor and key/value records. Configure with `PluginHost::with_version_broker`; a missing source or mismatch is a loud load error.

## How to change it

- Dependencies live in `lodestone_wasm_host::Dependencies`; use the manifest's authoritative `name`, not the module's reported one. The graph planner is in `lodestone_wasm_host::manifest`, and `PluginHost` owns reverse teardown so every replacement path unloads the same way.
- Keep the schema and `PluginGrantPolicy` matching in `lodestone::wasm_plugins` aligned; a grant identity must include both path and name, since making either optional widens a grant to siblings. Add capability names to `lodestone_wasm_host::Capability` first, keeping unknown-name rejection.
- A new broker consumer updates the manifest's `[version-lock]` and the host descriptor together. Keep the WIT import by-value; handing out ECS handles, world guards, sockets or callbacks would cross the trust boundary.
- The flag is parsed in `lodestone::config::Config::from_args`, applied in `app::runners::run_windowed_with_app`. Embeddings with a reload control must call `reload_from_directory_with_grant_file` rather than cache a stale `PluginGrantPolicy`; `PluginHost::stage_directory_reload` is the lower-level all-or-nothing builder for non-file policy sources. Do not route the file through a downstream app's pre-installed `WasmHostPlugin`, which is authoritative over its own policy.

## Configuration

No grant file by default. Opt in with `lodestone --plugin-grants /path/to/grants.json`; the flag is refused for headless and connect-only modes, which install no plugin host. Reload is a native embedding API, not a browser feature or file watcher. Discovery stays `plugins/` relative to the working directory, and `manifest_path` is relative to that directory, not the JSON file.

## Dependencies

`serde_json` for the file, `lodestone-wasm-host` for vocabulary, identity matching and loading. Native-only because Wasmtime is; `Config` holds only a path so the browser library avoids that dependency.
