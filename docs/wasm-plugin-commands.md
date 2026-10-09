# WASM plugin commands

## What it is

The native WASM host lets a guest own a root command through the same `lodestone_ecs::commands::CommandRegistry` that compiled-in plugins use. A guest declares roots from `init`, then receives the canonical command line via `on-command`.

## How it works

`lodestone_wasm_host::WasmHostPlugin` reads each guest's `PluginInfo.commands` in `Plugin::build` and installs a `PluginCommand` (root, aliases, description, optional permission) only if the manifest requested `commands:register` and policy granted it. The registry does slash removal, alias rewrite, permission pruning and dispatch before calling the guest.

- Empty `arguments`: the root plus one greedy `arguments` child, so `/example` and `/example anything` both reach the guest.
- Non-empty `arguments`: a typed sequential path. Types: word, quotable and greedy strings, integers, longs, floats, doubles, booleans, and closed `choices`. Each slot may carry static completion candidates.

The callback returns `success(i32)` or `failure(string)` (surfaced as a normal command failure). It runs under the same bounded fuel budget as a veto callback; a trap permanently fails that guest and yields a command failure.

The guest receives the canonical string and a copy-only `command-context`: `sender-name` always, plus, for contextual commands, the entity id and username, position, rotation, dimension, anchor and command level. It carries no `World`, ECS handle, UUID or callback, so dispatch under the command sink's world write guard cannot re-enter it.

Reload (`reload_from_directory_with_grants`, or `..._with_grant_file` for persisted grants) stages all guests and declarations first, then unregisters only the roots the conductor owned, swaps stores and registers the replacements. A failed reload leaves the old stores and roots active. Nothing watches the directory.

## How to change it

- Add WIT fields in `lodestone-wasm-host/wit/lodestone-plugin.wit`, then update `abi::lift_command_context`, `host::LoadedPlugin` and `conductor::register_wasm_commands`. A WIT change changes the ABI world string; guests must rebuild.
- `CommandRegistry` stays the owner of parsing, permissions and suggestions. Never give a guest a closure with `World` access.
- Use a closed `choices` argument to reject values outside a set; use `suggestions` for non-binding completions.
- Validate guest-returned strings before `PluginCommand::new`; they are untrusted. Duplicate roots across plugins are refused and logged while other guests still install.
- `tests/command_registration.rs` is the integration gate for permission pruning and reload.

## Configuration

```toml
capabilities = ["commands:register"]
```

`CapabilitySet::default_policy` withholds `commands:register`; an embedder must add `Capability::RegisterCommands`.

## Dependencies

`lodestone-wasm-host` (WIT boundary), `lodestone-ecs` (`CommandRegistry`), `lodestone-command` (argument parsers). The integrated server's command sink is the production route; remote and dedicated-server command paths are out of scope.
