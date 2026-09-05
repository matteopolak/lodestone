# WASM plugin intents

## What it is

The desktop WASM plugin host exposes copied local-player look, movement, block-breaking, and one-shot placement intents without giving a guest a world handle. A guest can install a yaw/pitch target, override one tick's input axes and buttons, start or abort a block break, request a block placement, swap a live menu slot with a hotbar position, throw one item or a complete stack from a live menu slot, drop its carried cursor stack, drop the selected stack, or swap the main- and off-hand stacks while native systems retain simulation and network ownership.

## How it works

`lodestone:plugin@0.18.0` provides `action.set-look(option<look-intent>)`, guarded by `act:look`, `action.set-movement(option<movement-intent>)`, guarded by `act:movement`, `action.set-break(option<break-intent>)`, guarded by `act:break`, `action.place-block(place-intent)`, guarded by `act:place`, `action.select-slot(hotbar-slot)`, guarded by `act:select-slot`, `action.inventory-click(inventory-click)`, guarded by `act:inventory-click`, `action.inventory-quick-move(u16)`, guarded separately by `act:inventory-quick-move`, `action.inventory-hotbar-swap(inventory-hotbar-swap)`, guarded separately by `act:inventory-hotbar-swap`, `action.inventory-throw(inventory-throw)`, guarded separately by `act:inventory-throw`, the no-argument `action.inventory-drop-cursor`, guarded separately by `act:inventory-drop-cursor`, `action.drop-selected-item(selected-item-drop-mode)`, guarded by `act:drop-selected-item`, and the no-argument `action.swap-item-with-offhand`, guarded by `act:swap-offhand`. Pickup/place has a copied `u16` slot plus a left/right button; quick move has only the `u16` slot, so the shell—not the guest—chooses the shift-click mode and its transfer ordering. Hotbar swap has a copied `u16` menu slot and `u8` hotbar index; the shell rejects indices outside `0..=8`, chooses the swap click mode, and validates the live menu. Slot throw has a copied `u16` menu slot plus an explicit `one`/`stack` mode; the shell validates the slot, chooses the throw button, and lets the live predictor own the slot mutation and drop outcome. Cursor drop has no slot or cursor payload: the shell verifies that its active live menu has a carried stack, then chooses the outside click itself. Selected-item drop carries only `one` or `stack`; the host queues the corresponding client action and the live client owns the selected-stack state and wire encoding. Offhand swap carries no stack or slot data; the host queues the corresponding client action and the live client remains the authority over both held stacks and protocol encoding. The shell validates each copied request against the active live menu and then calls `ClientHandle::menu_click`, so prediction, the veto, state id, changed-slot list, and egress remain owned by the client read model. `lodestone_wasm_host::abi::lower_action` turns look into `lodestone_ecs::player::LookIntent`; the conductor applies the final look request in load order during `TickSet::Intent`, before `apply_look_intent`.

For movement, the conductor runs after `lodestone_controller::ecs::compute_movement_intent` and before physics. It overwrites only copied axes and button state; item-use effects stay owned by the controller. Finite axes are clamped to `[-1, 1]`, and non-finite axes become neutral. Physics then resolves the request and the existing controller sends the ordinary movement and player-input actions. This is deliberately an intent rather than a raw packet: a guest cannot forge position, collision, or sequence state.

The guest output list is ordered. If multiple guest actions set look or set movement in one tick, the last request of each kind wins. `none` removes look ownership or leaves the normal controller input intact for movement. The focused integration gate builds separate guest artifacts, verifies a changed `PhysicsState`, and verifies that the ordinary movement sender reports the resulting look or input in that tick.

`set-break(some(break-intent))` installs or retargets a persistent `BreakIntent`; returning no break action leaves that claim alone, and `set-break(none)` releases it. The shell's existing `drive_mining` consumer owns reach and obstruction checks, live-state lookup, tool speed, vetoes, crack progress, local air prediction, the packet sequence, and the idempotent abort. Human attack input wins. `observe:break` supplies only changed `idle`, `progressing`, or finite rejection states per local-player session, so a long dig cannot turn into an unbounded every-tick event feed.

`place-block` is deliberately one-shot rather than a packet constructor or a component handle. The guest gives only a clicked block position and face. The conductor installs the last placement request from that tick on the local player before `TickSet::Send`; `lodestone_shell::interact::drive_placement` then runs its normal reach/obstruction, inventory, collision, veto, prediction, sequence, and egress path. Human use input continues to win under that lifecycle's existing rule.

`select-slot` is also one-shot. The guest provides an unsigned candidate slot; the conductor installs the final request from a tick as `SelectSlotIntent`, and `lodestone_shell::interact::drive_select_slot` later validates the `0..=8` range, ignores an already-selected value, updates `SelectedSlot`, and queues `SetCarriedItem` only for a real change. That shell-owned echo is the authority boundary: a guest never writes the selected component directly or manufactures a carried-item packet. There is no `observe:select-slot` capability or outcome event because selection has no world legality state: valid values always succeed, and an out-of-range or no-op request is deliberately consumed as a finite no-op.

`drop-selected-item` is the direct queue counterpart: the guest chooses only `one` or `stack`, and `lodestone_wasm_host::abi::lower_action` maps that mode to the corresponding `ClientAction`; it never receives or supplies the selected item.

`swap-item-with-offhand` is the corresponding no-argument direct queue action. It has its own `act:swap-offhand` grant because swapping held stacks mutates inventory state and is distinct from the arm animation capability; the live client owns the stacks and wire encoding.

`observe:place` grants one `event.place-outcome` per resolved `PlaceOutcome::generation` and per local-player session. Its finite status is `predicted`, `sent-unpredicted`, or a finite rejection reason. No idle event repeats every tick, no world handle crosses the ABI, and no arbitrary host error string is exposed. A reconnect is a new player identity, so a first result at generation `1` is not hidden by the previous session's generation `1`.

`observe:inventory` grants `event.inventory-slot-changed` only for the existing `ClientEvent::InventorySlotChanged` stream: native player-inventory slots outside an open container. Its `item-stack` is a copied canonical item key and count; item data components, open-container slots, and the cursor stay in the native session model. The client driver publishes the original `ClientEvent` once to `GameEvent`, which is also the native-plugin observation path, and the conductor filters that bus per guest. This adds no inventory cache, packet constructor, or write path.

The filesystem imports are separate capability columns. `filesystem.read-file` is guarded by
`fs:read`, while `filesystem-write.write-file` is guarded independently by `fs:write`; granting one
does not link the other interface. Both require `PluginHost::with_filesystem_root`, and every write
must target an existing directory below that root. Parent traversal and symlink escapes are refused,
and the host records attempted paths and bytes for diagnostics. `fs:write` is an import capability,
so a guest that references it without the grant fails during instantiation rather than receiving a
late data-flow denial.

## How to change it

Add a new copied intent arm to `crates/lodestone-wasm-host/wit/lodestone-plugin.wit`, give it an explicit capability in `capability.rs`, and make `abi::lower_action` exhaustive over it. Route it through `conductor::PendingWasmIntents` only when it has an existing ECS consumer with a deliberate schedule edge; do not lower an intent into a raw `ClientAction`. Inventory clicks are the narrow exception: `PendingWasmMenuClicks` is drained after the ECS guard because `ClientHandle::menu_click` alone owns the mutable menu predictor. For a one-shot outcome, keep a bounded generation cursor in the host as `LoadedPlugin::observe_place_outcome`; for a continuous lifecycle, keep a per-session status-edge cursor like `LoadedPlugin::observe_break_outcome`. Do not turn either component poll into an every-tick event stream.

Any WIT change requires increasing `host::ABI_WORLD`, rebuilding guests, and updating the sample plugin's declared ABI. Keep the integration test on a composed `lodestone_app::client_app()` so it proves the production consumer chain rather than a standalone component write. Menu actions remain the narrow exception to the normal intent queue: `PendingWasmMenuClicks` is a fixed-capacity copied queue drained after the ECS guard, and the shell's live `ClientHandle::menu_click` owns all menu state and packet construction.

## Configuration

The default host policy withholds `act:look`, `act:movement`, `act:break`, `observe:break`, `act:place`, `observe:place`, `act:select-slot`, `act:inventory-click`, `act:inventory-quick-move`, `act:inventory-hotbar-swap`, `act:inventory-drop-cursor`, `act:drop-selected-item`, `act:swap-offhand`, `observe:inventory`, `fs:read`, and `fs:write`. A guest manifest must request every capability it uses; a request alone never changes the policy.

```toml
capabilities = ["act:movement"]
```

```toml
capabilities = ["act:place", "observe:place"]
```

```toml
capabilities = ["act:break", "observe:break"]
```

```toml
capabilities = ["act:select-slot"]
```

```toml
capabilities = ["act:inventory-click"]
```

```toml
capabilities = ["act:inventory-quick-move"]
```

```toml
capabilities = ["act:inventory-hotbar-swap"]
```

```toml
capabilities = ["act:inventory-throw"]
```

```toml
capabilities = ["act:inventory-drop-cursor"]
```

```toml
capabilities = ["act:swap-offhand"]
```

```toml
capabilities = ["observe:inventory"]
```

```toml
capabilities = ["fs:write"]
```

The write root is host configuration, not guest input. Configure it with
`PluginHost::with_filesystem_root`; a missing root or a path outside it makes the write return an
error. Directory creation is intentionally not exposed, so a plugin can only replace or create a
file in a directory the operator prepared.

For desktop directory discovery, use `PluginGrantPolicy` to add a narrow exception
to the shell's unchanged fail-closed baseline. `PluginIdentity` contains the path
to `plugin.toml` relative to the discovered `plugins/` directory **and** the
manifest's `name`. Both fields must match. Do not key a grant by the module's
reported `init` name or by a `.wasm` filename: neither identifies the configured
installation instance.

```rust
use lodestone_wasm_host::{Capability, CapabilitySet, PluginGrantPolicy, PluginIdentity};

let mut grants = PluginGrantPolicy::default();
grants.grant(
    PluginIdentity::new("builder/plugin.toml", "builder"),
    CapabilitySet::from_iter([Capability::ActPlace, Capability::ObservePlace]),
);
lodestone::wasm_plugins::install_from_directory_with_grants(
    &mut app,
    std::path::Path::new("plugins"),
    &grants,
)?;
```

`PluginHost::load_directory_with_grants` adds that pair only while loading the
matching manifest; all unlisted plugins retain the host baseline. It recomputes
the path-relative match each time discovery runs, so a host rebuilt for reload
uses the same policy. Changing grants does not alter a running guest's authority:
reload the host deliberately. `install_from_directory` remains the convenient
empty-grants helper for shell and headless-library consumers that want no
exceptions.

## Dependencies

This feature depends on `lodestone-wasm-host` for the component boundary and `lodestone-ecs` for the local-player intents, outcomes, tick schedule, and action queue. `lodestone-controller` remains the producer of normal input and the owner of movement-action emission; `lodestone-shell` remains the owner of breaking and placement validation, prediction, sequences, and protocol egress.
