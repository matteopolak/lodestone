# WASM plugin intents

## What it is

The desktop WASM plugin host exposes copied local-player look, movement, and one-shot placement intents without giving a guest a world handle. A guest can install a yaw/pitch target, override one tick's input axes and buttons, or request a block placement while native systems retain simulation and network ownership.

## How it works

`lodestone:plugin@0.8.0` provides `action.set-look(option<look-intent>)`, guarded by `act:look`, `action.set-movement(option<movement-intent>)`, guarded by `act:movement`, and `action.place-block(place-intent)`, guarded by `act:place`. `lodestone_wasm_host::abi::lower_action` turns look into `lodestone_ecs::player::LookIntent`; the conductor applies the final look request in load order during `TickSet::Intent`, before `apply_look_intent`.

For movement, the conductor runs after `lodestone_controller::ecs::compute_movement_intent` and before physics. It overwrites only copied axes and button state; item-use effects stay owned by the controller. Finite axes are clamped to `[-1, 1]`, and non-finite axes become neutral. Physics then resolves the request and the existing controller sends the ordinary movement and player-input actions. This is deliberately an intent rather than a raw packet: a guest cannot forge position, collision, or sequence state.

The guest output list is ordered. If multiple guest actions set look or set movement in one tick, the last request of each kind wins. `none` removes look ownership or leaves the normal controller input intact for movement. The focused integration gate builds separate guest artifacts, verifies a changed `PhysicsState`, and verifies that the ordinary movement sender reports the resulting look or input in that tick.

`place-block` is deliberately one-shot rather than a packet constructor or a component handle. The guest gives only a clicked block position and face. The conductor installs the last placement request from that tick on the local player before `TickSet::Send`; `lodestone_shell::interact::drive_placement` then runs its normal reach/obstruction, inventory, collision, veto, prediction, sequence, and egress path. Human use input continues to win under that lifecycle's existing rule.

`observe:place` grants one `event.place-outcome` per resolved `PlaceOutcome::generation` and per local-player session. Its finite status is `predicted`, `sent-unpredicted`, or a finite rejection reason. No idle event repeats every tick, no world handle crosses the ABI, and no arbitrary host error string is exposed. A reconnect is a new player identity, so a first result at generation `1` is not hidden by the previous session's generation `1`.

## How to change it

Add a new copied intent arm to `crates/lodestone-wasm-host/wit/lodestone-plugin.wit`, give it an explicit capability in `capability.rs`, and make `abi::lower_action` exhaustive over it. Route it through `conductor::PendingWasmIntents` only when it has an existing ECS consumer with a deliberate schedule edge; do not lower an intent into a raw `ClientAction`. For a one-shot outcome, keep a bounded generation cursor in the host as `LoadedPlugin::observe_place_outcome`; do not turn a component poll into an every-tick event stream.

Any WIT change requires increasing `host::ABI_WORLD`, rebuilding guests, and updating the sample plugin's declared ABI. Keep the integration test on a composed `lodestone_app::client_app()` so it proves the production consumer chain rather than a standalone component write.

## Configuration

The default host policy withholds `act:look`, `act:movement`, `act:place`, and `observe:place`. An embedding host must grant the corresponding capability, and the guest manifest must request it.

```toml
capabilities = ["act:movement"]
```

```toml
capabilities = ["act:place", "observe:place"]
```

## Dependencies

This feature depends on `lodestone-wasm-host` for the component boundary and `lodestone-ecs` for the local-player intents, outcomes, tick schedule, and action queue. `lodestone-controller` remains the producer of normal input and the owner of movement-action emission; `lodestone-shell` remains the owner of placement validation, prediction, and protocol egress.
