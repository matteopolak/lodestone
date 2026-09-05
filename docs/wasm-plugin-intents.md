# WASM plugin intents

## What it is

The desktop WASM plugin host exposes copied local-player look ownership through `set-look`. A guest can install a yaw/pitch target or pass `none` to return control to regular input without receiving a world handle.

## How it works

`lodestone:plugin@0.6.0` adds `action.set-look(option<look-intent>)`, guarded by the data-flow capability `act:look`. `lodestone_wasm_host::abi::lower_action` turns the guest value into `lodestone_ecs::player::LookIntent`; `WasmHostPlugin` applies the final guest request in load order during `TickSet::Intent`, before `apply_look_intent`.

The existing local-player pipeline then clamps pitch, commits the rotation to `PhysicsState`, derives movement relative to that facing, and produces the normal movement action. This is deliberately an intent rather than a raw packet: a guest cannot forge position, collision, or sequence state.

The guest output list is ordered. If multiple guest actions set look in one tick, the last request wins; `none` removes the component and hands rotation back to normal input. The focused integration gate builds a separate guest artifact, asserts the changed `PhysicsState`, and verifies that the ordinary movement sender reports the same rotation in that tick.

## How to change it

Add a new copied intent arm to `crates/lodestone-wasm-host/wit/lodestone-plugin.wit`, give it an explicit capability in `capability.rs`, and make `abi::lower_action` exhaustive over it. Route it through `conductor::PendingWasmIntents` only when it has an existing ECS consumer with a deliberate schedule edge; do not lower an intent into a raw `ClientAction`.

Any WIT change requires increasing `host::ABI_WORLD`, rebuilding guests, and updating the sample plugin's declared ABI. Keep the integration test on a composed `lodestone_app::client_app()` so it proves the production consumer chain rather than a standalone component write.

## Configuration

The default host policy withholds `act:look`. An embedding host must grant `Capability::ActLook`, and the guest manifest must request `act:look`.

```toml
capabilities = ["act:look"]
```

## Dependencies

This feature depends on `lodestone-wasm-host` for the component boundary and `lodestone-ecs` for the local-player `LookIntent`, tick schedule, and action queue. `lodestone-controller` remains the producer of normal input and the owner of movement-action emission.
