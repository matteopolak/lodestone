# Plugin entity observation

## What it is

`observe:entities` gives a WASM plugin a typed, copied view of remote-entity lifecycle and selected player state. Native plugins already consume the same decoded `GameEvent(ClientEvent)` stream; the WASM host mirrors the useful entity subset without exposing an ECS entity, a component borrow, or a UUID.

## How it works

`WasmHostPlugin` reads the production `GameEvent` bus during `TickSet::Intent`. For an observing guest, `lodestone_wasm_host::abi::lift_entity_events` turns spawn, movement, velocity, removal, health metadata, equipment updates, and local-player teleports into WIT `event` values. A guest-specific generation ledger increments each time an observed network id is spawned and marks it inactive on removal. Updates for a lifecycle whose spawn the guest did not observe are withheld, rather than guessing that a reused id is continuous.

The WIT payloads contain only values: an `entity-identity { entity-id, generation }`, resource-key string, coordinates, rotation, optional velocity, health, and up to the protocol's defined equipment slots. A relative movement stays a relative delta. A local teleport retains its relative-component flags. The stream is observation-only; mutations still use the separately capability-gated action and intent routes, each owned by their existing simulation and egress consumers.

`tests/entity_observation.rs` composes the real client app, verifies a native `GameEvent` reader observes the spawn, and proves a separately built guest returns a witness through the real `ActionQueue`.

## How to change it

Add a new entity observation in three places: the `types.event` WIT variant, `Capability::ObserveEntities`'s lift in `abi::lift_entity_events`, and an exact mapping test. Preserve the lifecycle rule: events that name an unobserved or already removed id must not fabricate a generation. If a capability requires persistent current-world state rather than a packet-derived update, add a bounded copied snapshot API instead of querying ECS while guest code runs.

Any WIT change requires a new `ABI_WORLD` package version and a rebuilt guest. Update the example guest and the production gate together; an old guest must fail its ABI check loudly rather than interpret the changed event layout.

## Configuration

Plugins request `observe:entities` in `plugin.toml`. `CapabilitySet::default_policy` grants this copied observation capability; an embedding may still omit it from an individual guest's grant set. It grants no action, world read, or mutable entity access.

## Dependencies

The native side depends on `lodestone_ecs::events::GameEvent` and `lodestone_model::ClientEvent`. The WASM mapping is generated from `crates/lodestone-wasm-host/wit/lodestone-plugin.wit`, then driven by `lodestone_wasm_host::conductor::drive_wasm_plugins`.
