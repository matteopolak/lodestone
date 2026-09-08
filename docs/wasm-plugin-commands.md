# WASM plugin commands

## What it is

The native WASM host can expose a guest-owned root command through the same `lodestone_ecs::commands::CommandRegistry` used by compiled-in plugins. A guest declares command roots from its `init` export, then receives the canonical command line through `on-command` when a permitted player invokes one.

## How it works

`lodestone_wasm_host::WasmHostPlugin` reads each loaded guest's `PluginInfo.commands` during its `Plugin::build` call. A declaration is installed only when its manifest requested `commands:register` and the host policy granted it. The host turns the declaration into a `PluginCommand` with the declared root, aliases, description, and optional permission. The native registry performs its usual leading-slash removal, alias rewrite, permission pruning, and dispatch before it invokes the WASM callback.

An empty `arguments` declaration keeps the compatibility form: each registered root has an executable root and one executable greedy `arguments` child. That means both `/example` and `/example arbitrary tail` reach the guest. A non-empty declaration instead creates a sequential typed argument path. The ABI supports word, quotable, and greedy strings, integers, long integers, floats, doubles, booleans, and strict closed choices. Each slot can also carry static completion candidates; the native registry applies its normal permission and prefix filtering. The final executable node receives the canonical input without a leading slash. The callback returns either `success(i32)`, preserving the normal command-result value, or `failure(string)`, which reaches the command sender as the usual command failure. The host gives the callback the same bounded synchronous fuel budget used for a veto callback. A trap permanently fails that guest and produces a command failure rather than unwinding through the command dispatcher.

The callback receives the canonical string and a copy-only `command-context`. `sender-name` is always
present. A direct root has no execution value; a contextual command additionally carries the selected
entity's numeric id and username, position, rotation, dimension, anchor, and resolved command level.
The native registry has already rewritten aliases and checked the declared permission before this data
crosses the boundary. It deliberately carries no `World`, ECS handle, UUID, or callback, so dispatching
while the native command sink holds the client world write guard cannot re-enter that guard.

The shell's explicit directory reload path stages every guest and its command declarations before it
commits. On success it unregisters only the roots previously owned by the WASM conductor, swaps the
guest stores, and registers the replacement declarations. A failed reload leaves both the old stores
and their command roots active; the shipped shell does not watch the directory automatically.

## How to change it

Add WIT command fields in `lodestone-wasm-host/wit/lodestone-plugin.wit`, then update
`lodestone_wasm_host::abi::lift_command_context`, the generated type handling in
`lodestone_wasm_host::host::LoadedPlugin`, and the bridge in
`lodestone_wasm_host::conductor::register_wasm_commands`. Changing WIT changes the ABI world string
and requires guests to rebuild. Use the shell's `reload_from_directory_with_grants` for a deliberate
runtime replacement; use `reload_from_directory_with_grant_file` when the grants are persisted.

Keep `CommandRegistry` as the parser, permission, and suggestion owner. In particular, do not let a
guest install a closure that has direct `World` access. The guest receives the canonical input string,
so it can interpret the values after the registry has enforced the declared schema without receiving
an ECS or world handle. The compatibility greedy tail is a narrow real command path, not a substitute
for a declared typed schema. Use a closed `choices` argument when values outside a finite set must be
rejected; use the separate `suggestions` list when completions are helpful but should not constrain
parsing.

Validate new declaration data before `WasmHostPlugin` calls `PluginCommand::new`: guest-returned strings are untrusted and must not reach an assertion or register silently unusable roots. Cross-plugin duplicate roots are refused by `CommandRegistry` and logged while other guests continue installing.

## Configuration

Guests declare `commands:register` in `plugin.toml` and return `command-spec` entries from `init`. The default `CapabilitySet::default_policy` deliberately withholds `commands:register`; an embedding host must explicitly add `Capability::RegisterCommands` to its policy to permit it.

```toml
capabilities = ["commands:register"]
```

## Dependencies

This bridge depends on `lodestone-wasm-host` for the WIT component boundary, `lodestone-ecs` for
`CommandRegistry`, permission dispatch, and completion filtering, and `lodestone-command` for the
primitive argument parsers. The integrated local server's existing command sink is the production
route from a player command to that registry; remote and dedicated-server command paths remain
outside this client-owned capability.
