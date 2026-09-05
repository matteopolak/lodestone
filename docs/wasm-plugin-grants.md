# WASM plugin grants

## What it is

`--plugin-grants <FILE>` is the shipped shell's opt-in, persisted configuration for granting capabilities beyond the WASM host's fail-closed baseline. It applies additions to one discovered plugin instance, identified by both its discovery-root-relative `plugin.toml` path and that manifest's `name`.

## How it works

The shell reads the JSON file once, immediately before it asks `lodestone::wasm_plugins::install_from_directory_with_grants` to discover `plugins/`. Each entry becomes a `lodestone_wasm_host::PluginIdentity` plus a `CapabilitySet`; the host matches both fields while it evaluates each manifest. An unmatched plugin keeps only `CapabilitySet::default_policy`.

```json
{
  "grants": [
    {
      "manifest_path": "trusted/plugin.toml",
      "name": "trusted",
      "capabilities": ["fs:read", "act:place", "observe:place"]
    }
  ]
}
```

`manifest_path` must be relative to the discovery root, end in `plugin.toml`, and contain no `.` or `..` segments. The configuration parser rejects unknown fields, duplicate `(manifest_path, name)` pairs, unreadable or malformed files, and every capability name that `lodestone_wasm_host::Capability::parse` does not recognise. It never silently drops a request or applies a partial configuration.

The policy is not watched. The shipped desktop launcher reads it at startup, while a native embedding can deliberately call `lodestone::wasm_plugins::reload_from_directory_with_grant_file` to re-read the file and replace its installed guest stores. Each reload parses the complete file before staging any guest, then re-reads every manifest and module against that new policy. A malformed policy, denied capability, malformed manifest, invalid module, or command collision rejects the entire replacement and leaves the active guests in place. This prevents a changed file from changing a running plugin's authority without an explicit, revalidated reload boundary.

Each `plugin.toml` may also declare `[dependencies]` with typed `required` and `optional` name lists. Required dependencies must be present in the same discovery directory; optional dependencies are ordering edges only when present. Discovery topologically sorts the graph, using priority, manifest name, and manifest path as stable tie-breakers among ready plugins. Missing required dependencies, duplicate names, and cycles are reported deterministically. Startup can still report a bad entry while loading independent valid entries, but transactional reload rejects any graph error before committing a candidate.

At a successful reload, old guest stores are dropped in reverse dependency/load order and the staged stores take their place in the same deterministic priority/name order. The conductor removes only command roots that it previously installed, then registers the replacement roots; it never clears a neighbouring native plugin's command. Guest-owned pending intents are discarded at the commit boundary, and the new stores begin with empty placement/break outcome cursors, so no intent or cursor from an unloaded guest survives.

## How to change it

Dependency declarations live in `lodestone_wasm_host::Dependencies`; keep names aligned with the manifest's authoritative `name`, not the module's reported name.

Keep the persisted schema and `PluginGrantPolicy` matching rule in `lodestone::wasm_plugins` aligned. A grant identity must continue to include both the root-relative manifest path and manifest name; making either optional would widen a grant to a sibling plugin. Add capability names through `lodestone_wasm_host::Capability` first, then keep the parser's unknown-name rejection intact.

The shell-facing flag belongs in `lodestone::config::Config::from_args`, and `app::runners::run_windowed_with_app` is the shipped startup point. An embedding that exposes a reload control must call `reload_from_directory_with_grant_file`, not cache a previous `PluginGrantPolicy` after the file changes. `lodestone_wasm_host::PluginHost::stage_directory_reload` is the lower-level all-or-nothing candidate builder for a non-file policy source. Do not route the persisted file through a downstream app's pre-installed `WasmHostPlugin`: that host is intentionally authoritative and its embedding controls its own policy.

## Configuration

No grant file is loaded by default. Start the windowed client with `lodestone --plugin-grants /path/to/grants.json` to opt in; the flag is refused for headless and connect-only modes because neither installs the plugin host. Reload is a native embedding API, not a browser feature or background file watcher. The plugin discovery directory remains `plugins/` relative to the working directory; `manifest_path` is relative to that directory, not relative to the JSON file.

## Dependencies

This configuration uses `serde_json` for the file shape and `lodestone-wasm-host` for capability vocabulary, identity matching, and loading. It is native-only because the Wasmtime host is native-only; the `Config` field is only a path so the browser library remains free of that dependency.

Manifest dependency declarations use `serde` through `lodestone-wasm-host`; they do not add a runtime or network dependency. The host's graph planner is in `lodestone_wasm_host::manifest`, and reverse teardown is owned by `PluginHost` so every replacement path shares the same unload rule.
