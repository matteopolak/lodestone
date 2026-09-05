# WASM plugin commands

## What it is

The native WASM host can expose a guest-owned root command through the same `lodestone_ecs::commands::CommandRegistry` used by compiled-in plugins. A guest declares command roots from its `init` export, then receives the canonical command line through `on-command` when a permitted player invokes one.

## How it works

`lodestone_wasm_host::WasmHostPlugin` reads each loaded guest's `PluginInfo.commands` during its `Plugin::build` call. A declaration is installed only when its manifest requested `commands:register` and the host policy granted it. The host turns the declaration into a `PluginCommand` with the declared root, aliases, description, and optional permission. The native registry performs its usual leading-slash removal, alias rewrite, permission pruning, and dispatch before it invokes the WASM callback.

Each registered root has an executable root and one executable greedy `arguments` child. That means both `/example` and `/example arbitrary tail` reach the guest; `on-command` receives the canonical input without a leading slash. The callback returns either `success(i32)`, preserving the normal command-result value, or `failure(string)`, which reaches the command sender as the usual command failure. The host gives the callback the same bounded synchronous fuel budget used for a veto callback. A trap permanently fails that guest and produces a command failure rather than unwinding through the command dispatcher.

The callback receives only a string. It has no `World`, client ECS handle, command sender, or execution-context handle, so dispatching while the native command sink holds the client world write guard cannot re-enter that guard.

## How to change it

Add WIT command fields or callback context in `lodestone-wasm-host/wit/lodestone-plugin.wit`, then update the generated type handling in `lodestone_wasm_host::host::LoadedPlugin` and the bridge in `lodestone_wasm_host::conductor::register_wasm_commands`. Changing WIT changes the ABI world string and requires guests to rebuild.

Keep `CommandRegistry` as the parser and permission owner. In particular, do not let a guest install a closure that has direct `World` access. Typed WIT argument schemas, suggestions, and sender/context data are intentionally not represented yet; the single greedy tail is a narrow real command path, not a substitute for the native command-tree API.

Validate new declaration data before `WasmHostPlugin` calls `PluginCommand::new`: guest-returned strings are untrusted and must not reach an assertion or register silently unusable roots. Cross-plugin duplicate roots are refused by `CommandRegistry` and logged while other guests continue installing.

## Configuration

Guests declare `commands:register` in `plugin.toml` and return `command-spec` entries from `init`. The default `CapabilitySet::default_policy` deliberately withholds `commands:register`; an embedding host must explicitly add `Capability::RegisterCommands` to its policy to permit it.

```toml
capabilities = ["commands:register"]
```

## Dependencies

This bridge depends on `lodestone-wasm-host` for the WIT component boundary, `lodestone-ecs` for `CommandRegistry` and permission dispatch, and `lodestone-command` for the greedy tail parser. The integrated local server's existing command sink is the production route from a player command to that registry; remote and dedicated-server command paths remain outside this client-owned capability.
