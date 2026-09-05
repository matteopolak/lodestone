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

The policy is not watched and it cannot alter an already-loaded guest. Editing the file requires an explicit new launch (or an embedding that creates a fresh `PluginHost` and calls `install_from_directory_with_grants` again). This prevents a changed file from changing a running plugin's authority without a deliberate reload boundary.

## How to change it

Keep the persisted schema and `PluginGrantPolicy` matching rule in `lodestone::wasm_plugins` aligned. A grant identity must continue to include both the root-relative manifest path and manifest name; making either optional would widen a grant to a sibling plugin. Add capability names through `lodestone_wasm_host::Capability` first, then keep the parser's unknown-name rejection intact.

The shell-facing flag belongs in `lodestone::config::Config::from_args`, and `app::runners::run_windowed_with_app` is the only shipped application point. Do not route the persisted file through a downstream app's pre-installed `WasmHostPlugin`: that host is intentionally authoritative and its embedding controls its own policy.

## Configuration

No grant file is loaded by default. Start the windowed client with `lodestone --plugin-grants /path/to/grants.json` to opt in; the flag is refused for headless and connect-only modes because neither installs the plugin host. The plugin discovery directory remains `plugins/` relative to the working directory; `manifest_path` is relative to that directory, not relative to the JSON file.

## Dependencies

This configuration uses `serde_json` for the file shape and `lodestone-wasm-host` for capability vocabulary, identity matching, and loading. It is native-only because the Wasmtime host is native-only; the `Config` field is only a path so the browser library remains free of that dependency.
