# Plan: runtime plugin loading — wasm components, and the same plugin compiled in

## What it is

Runtime plugin loading through WebAssembly components, alongside compiled-in bevy plugins. `lodestone-wasm-host` (Wasmtime, component model, WIT) is shipped on the desktop client; the open work is the portable-plugin shim, richer ABI, and a server-side conductor.

## Decisions

- **Two power levels, one vocabulary.** A native plugin gets `&mut World`; a wasm guest gets only the intent doctrine's vocabulary: the `GameEvent` bus (observe), intent components and `ActionQueue` (act), outcome components (hear back). All of it is copy-shaped, so the doctrine doubles as the ABI.
- **A portable plugin is written against a bevy-free `lodestone-plugin-api`** and built two ways: a generated `NativeHost<P>` shim into an ordinary bevy plugin, or a wasm component. Nav-class workloads such as `lodestone-autopilot` stay native.
- **Wasmtime + component model + WIT**, `wit-bindgen` for Rust guests. Wasmer's WASIX and wasmi (no component model) were rejected for the desktop host. The WIT world is the ABI version unit.
- **Dynamic libraries are closed off**: Rust has no stable ABI and `TypeId` identity breaks across builds, giving silent wrong behaviour.
- **Browser loading is out of scope.** Wasmtime cannot run inside a wasm32 guest; the only routes are wasmi-in-wasm or jco transpilation, and the WIT ABI forecloses neither.

## What a guest cannot do

| limit | consequence |
|---|---|
| hold a borrow into the host `World` | everything is copied across linear memory |
| be a system | one conductor system per slot drives guests, ordered by manifest `EventPriority` tier; the conductor is the single `ActionQueue` writer |
| define component types other plugins see | cross-plugin state is host vocabulary only |
| block the tick | fuel preemption traps a runaway guest and marks it permanently failed |
| touch the socket task or GPU device | same as native plugins |

## Shipped

- `lodestone-wasm-host`: plugin directory scan, TOML manifest (capability policy, WIT world version, priority tier), capability-gated imports (absent `Linker` interface), `init` / `on-tick` / `on-task` callbacks, fuel preemption, failure isolation.
- `lodestone-app::client_app()` is the renderer-free composition; `Sim::client_app()` adds shell-coupled plugins; `run_with_app` takes a caller's `App`. The native windowed runner installs the conductor and scans cwd-relative `plugins/`.
- Epoch preemption is not enabled (it needs a watchdog).

## Open work

- **Per-tick cost measurements** on an idle machine: one `on-tick` round trip, batched entity snapshots at 1k/5k/20k, per-guest overhead at 1/5/20 guests, per-`Store` memory. A result that conflicts with the batching model is a design signal.
- **Dual path**: `lodestone-plugin-api` plus `NativeHost`. Conformance gate: the same plugin source built both ways, run on one recorded event stream, yields identical action sequences. First candidates are the event logger plus a chat responder, not autopilot.
- **ABI extension** (versioned): intent install/remove, outcome delivery, component mirroring, portable command declaration.
- **Manifest dependencies, wasm hot reload and state carry-over.** Native hot reload is rejected.
- **Server-side conductor**, once the server has a registration point (proposal queue and `Adjudicate` window).
- Owner decisions: in-game plugin listing and grant prompts, signing, a cross-plugin key-value channel, whether the shell bundles any plugins.

## How to change it

- ABI changes go through the vendored `.wit` files under `crates/lodestone-wasm-host/wit/` and a world version bump; the host rejects guests built for a different world.
- Keep `lodestone-wasm-host` version-free and out of the wasm32 client graph.

## Dependencies

- Pinned `wasmtime` (no `wasmtime-wasi`), vendored WIT, `lodestone-ecs`, `lodestone-model`.
- See [the plugin API](../plugin-api.md) for the doctrine and [WASM embedding](../wasm-embedding.md).
