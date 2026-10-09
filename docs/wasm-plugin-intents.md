# WASM plugin intents

## What it is

The desktop WASM plugin host exposes copied local-player look, movement, block-breaking, one-shot placement and bounded inventory intents without giving a guest a world handle. A guest can request clicks, bulk transfers, pickup-all double clicks, hotbar swaps, drops, held-item actions or respawn, while native systems keep ownership of menu, simulation and network state.

## How it works

`lodestone:plugin@0.29.0` provides these actions, each behind its own default-denied capability:

| action | capability | payload |
|---|---|---|
| `set-look(option<look-intent>)` | `act:look` | look intent |
| `set-movement(option<movement-intent>)` | `act:movement` | axes and buttons |
| `set-break(option<break-intent>)` | `act:break` | persistent claim |
| `place-block(place-intent)` | `act:place` | block position and face, one-shot |
| `select-slot(hotbar-slot)` | `act:select-slot` | candidate slot, one-shot |
| `inventory-click(inventory-click)` | `act:inventory-click` | `u16` slot plus left/right |
| `inventory-quick-move(u16)` | `act:inventory-quick-move` | slot only (shell picks shift-click mode and order) |
| `inventory-double-click(u16)` | `act:inventory-double-click` | slot only (the live carried stack decides matches) |
| `inventory-hotbar-swap(...)` | `act:inventory-hotbar-swap` | `u16` slot, `u8` hotbar index (`0..=8`; shell picks click mode) |
| `inventory-throw(...)` | `act:inventory-throw` | `u16` slot, `one`/`stack` |
| `inventory-drop-cursor` | `act:inventory-drop-cursor` | none (shell verifies a carried stack, picks the outside click) |
| `drop-selected-item(mode)` | `act:drop-selected-item` | `one`/`stack` |
| `swap-item-with-offhand` | `act:swap-offhand` | none |
| `release-use-item` | `act:release-use-item` | none (release can commit an effect) |
| `stab` | `act:stab` | none (not covered by `act:interact`) |
| `respawn` | `act:respawn` | none (server adjudicates) |
| `disconnect` | `act:disconnect` | none (no transport handle) |

`send-chat` (`act:chat`, conversational text) and `send-command` (`act:command`, separately denied since commands mutate server state) share the ordered client action queue.

**Authority.** Actions enter only the existing client action queue. For inventory, the shell validates each copied request against the active live menu and calls `ClientHandle::menu_click`, so prediction, veto, state id, changed slots and egress stay with the client read model; the guest never supplies stacks, cursors, hands, items or packets. `lodestone_wasm_host::abi::lower_action` lowers look to `lodestone_ecs::player::LookIntent`, applied in load order (last wins per kind) during `TickSet::Intent` before `apply_look_intent`. `PendingWasmMenuClicks` is a fixed-capacity copied queue drained after the ECS guard, the exception to the normal intent queue, because only `ClientHandle::menu_click` owns the mutable menu predictor.

**Movement** runs after `lodestone_controller::ecs::compute_movement_intent` and before physics, overwriting only copied axes and buttons; finite axes clamp to `[-1, 1]`, non-finite become neutral, and item-use effects stay with the controller. `none` leaves normal input intact. It is an intent, not a packet: a guest cannot forge position, collision or sequence state.

**Break.** `set-break(some(..))` installs or retargets a persistent `BreakIntent`; omitting the action leaves the claim, `set-break(none)` releases it. `drive_mining` owns reach and obstruction, tool speed, vetoes, crack progress, local air prediction, sequence and abort. Human attack input wins. `observe:break` supplies only changed `idle`, `progressing` or finite rejection states per session.

**Place.** The conductor installs the last request of a tick on the local player before `TickSet::Send`; `lodestone_shell::interact::drive_placement` runs the normal reach, inventory, collision, veto, prediction, sequence and egress path (human use input wins). `observe:place` emits one `event.place-outcome` per `PlaceOutcome::generation` and session (`predicted`, `sent-unpredicted` or a finite rejection reason). No idle events or error strings; a reconnect is a new identity, so generation `1` is not hidden by the previous session's.

**Select slot.** The conductor installs the final request as `SelectSlotIntent`; `drive_select_slot` validates `0..=8`, ignores an already-selected value, updates `SelectedSlot` and queues `SetCarriedItem` only on a real change. There is no observer capability: valid values always succeed, and an invalid or no-op request is a consumed finite no-op.

**Inventory observation.** `observe:inventory` copies `event.inventory-slot-changed` (player slots outside a container) and container content, single slot, menu-local data, open and close, mount-screen open, held-slot and cursor events as value-only records of window or container identity plus canonical item key and count (title and type flattened to strings). The driver publishes each `ClientEvent` once to `GameEvent`, and the WASM conductor filters that bus per guest; no cache or write path is added. `GameEvent::inventory_menu` carries the full `ItemStack` (including `ItemComponents::has_unmodeled`); the WASM record stops at key and count, so extending it must copy the full component-bearing stack and every lifecycle payload rather than build a second partial cache. Tests: `crates/lodestone-ecs/tests/inventory_menu_observation.rs`, the `inventory-menu` sample guest, and a lift test with a denied-event control.

**Filesystem.** `filesystem.read-file` needs `fs:read` and `filesystem-write.write-file` needs `fs:write`; granting one does not link the other. Both need `PluginHost::with_filesystem_root`, writes must target an existing directory under the root (parent traversal and symlink escapes refused, no directory creation), and attempted paths and bytes are recorded. `fs:write` is an import capability: a guest referencing it without the grant fails at instantiation.

## How to change it

Add an intent arm to `crates/lodestone-wasm-host/wit/lodestone-plugin.wit`, give it a capability in `capability.rs`, and make `abi::lower_action` exhaustive. Route through `conductor::PendingWasmIntents` only when an ECS consumer exists with a deliberate schedule edge; never lower into a raw `ClientAction`. For a one-shot outcome keep a bounded generation cursor (`LoadedPlugin::observe_place_outcome`); for a continuous lifecycle a per-session status-edge cursor (`observe_break_outcome`); never an every-tick stream. Any WIT change bumps `host::ABI_WORLD`, rebuilds guests and updates the sample plugin's declared ABI. Keep the integration test on a composed `lodestone_app::client_app()` so it proves the production consumer chain.

## Configuration

The default host policy withholds every capability above plus `observe:break`, `observe:place`, `observe:inventory`, `fs:read` and `fs:write`. A manifest must request each capability it uses (`capabilities = ["act:place", "observe:place"]`), and a request alone changes nothing. The write root is host configuration via `PluginHost::with_filesystem_root`.

For directory discovery, `PluginGrantPolicy` adds narrow exceptions to the fail-closed baseline. `PluginIdentity` is the path to `plugin.toml` relative to the discovered `plugins/` directory plus the manifest `name`; both must match (not the module's `init` name or a `.wasm` filename).

```rust
let mut grants = PluginGrantPolicy::default();
grants.grant(
    PluginIdentity::new("builder/plugin.toml", "builder"),
    CapabilitySet::from_iter([Capability::ActPlace, Capability::ObservePlace]),
);
lodestone::wasm_plugins::install_from_directory_with_grants(&mut app, Path::new("plugins"), &grants)?;
```

`PluginHost::load_directory_with_grants` applies the pair only while loading the matching manifest and recomputes the match each discovery. Changing grants does not alter a running guest; reload deliberately. `install_from_directory` is the empty-grants helper.

## Dependencies

`lodestone-wasm-host` (component boundary), `lodestone-ecs` (intents, outcomes, tick schedule, action queue), `lodestone-controller` (normal input and movement-action emission), `lodestone-shell` (break and placement validation, prediction, sequences, egress).
