# WASM raw packet observation

## What it is

The WASM plugin host can receive bounded, read-only copies of inbound and outbound protocol packets. Each observation carries the negotiated protocol number, connection phase, packet id, and packet body without transport framing.

## How it works

Plugins request `observe:packets`. The host installs both raw ECS buses for the composed client and applies per-tick packet and byte limits before any payload is copied. The conductor reads the admitted messages, lifts them into `event.raw-inbound-packet` or `event.raw-outbound-packet`, and passes owned component-model values to the guest. The adapter and transport retain sole ownership of decoding, encoding, ordering, and transmission.

When no loaded guest has the capability, the buses remain present for a stable conductor system parameter but use zero admission limits, so no packet body is copied or retained.

## How to change it

Update `crates/lodestone-wasm-host/wit/lodestone-plugin.wit` and `src/abi.rs` together when adding packet metadata. Keep payloads bounded at the ECS admission point; a guest-side memory limit is not a substitute for a host-side queue bound. Update `Capability::ALL`, the default policy, and the sample plugin manifest when adding a capability.

The protocol number is deliberately a primitive at this version-free boundary. Concrete version-family packet ids must remain in the adapter crates; a plugin that needs family-specific semantics should use the separately gated version broker.

## Configuration

`observe:packets` is granted by the normal WASM policy and can be removed from a host policy. Native callers can select `RawPacketBusPlugin::with_limits` and `OutboundRawPacketBusPlugin::with_limits` independently. Defaults admit at most 256 packets and 1 MiB of payload per game tick per direction.

## Dependencies

The feature uses `lodestone_ecs` raw message buses, `lodestone_client::SharedState` packet hooks, and the generated Wasmtime component bindings from the WASM host WIT file. It does not depend on a protocol-family crate.
