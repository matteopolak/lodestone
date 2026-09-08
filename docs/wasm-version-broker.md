# WASM version-locked broker

## What it is

The privileged `version:broker` capability gives a WASM plugin a small, typed import for data selected by the host's negotiated protocol family. It is the sandboxed counterpart to the native version-locked adapter escape hatch: a plugin must declare an exact family, protocol, and broker ABI, and the host refuses it before reading or compiling the module when that identity does not match.

The broker returns owned WIT records only. It does not expose a protocol adapter, packet bytes, registry handle, ECS borrow, socket, callback, or raw host pointer.

## How it works

The WIT `version-broker` interface provides a copied `descriptor` record and a copied `value` record. The host links that interface only when the manifest requests `version:broker` and the capability policy grants it. The host-side [`VersionBroker`](../crates/lodestone-wasm-host/src/version_broker.rs) trait supplies the descriptor and answers the provider's finite key vocabulary.

The manifest's `[version-lock]` table is required for a broker consumer:

```toml
capabilities = ["log", "version:broker"]

[version-lock]
family = "v26-2"
protocol = 776
abi = "lodestone:version-broker@0.1"
```

The loader compares all three fields exactly. A capability denial wins before broker lookup; a granted capability with no lock, no selected source, or a mismatched identity is also refused. This keeps a plugin from silently attaching version-shaped assumptions to a neighbouring protocol.

The shipped native shell installs [`RegistryVersionBroker`](../crates/lodestone-shell/src/wasm_plugins.rs) from the same registry adapter selected for `Config::protocol`. Its documented keys are `protocol`, `release-names`, `block-name/<state-id>`, and `block-hardness/<state-id>`. Their values are copied strings; an unknown or malformed key returns no value. The broker is absent from builds with no matching registry family, so a requesting plugin fails closed.

## How to change it

Keep the WIT interface and the host trait in lockstep. Any change to the WIT package requires a main-world ABI bump and a matching guest rebuild. Add a new provider value only when it can be expressed as bounded copied data; do not add adapter, registry, world, network, or callback handles.

When extending the shell provider, update its key constants, the allowlisted lookup implementation, this document, and the production broker test. Keep identity validation in the loader before module bytes are read. The capability remains privileged and must not be added to the default policy.

The native escape hatch remains separate: `lodestone-registry::plugin::VersionLockedPlugin` can wrap a concrete native adapter, while this WASM surface deliberately cannot hand a concrete version type to guest code.

## Configuration

- `version:broker` is a manifest capability and is withheld by `CapabilitySet::default_policy`.
- `[version-lock]` contains `family`, `protocol`, and `abi`; all are required when the capability is requested.
- The shell selects the broker from its configured protocol and compiled registry features. No environment variable or filesystem setting widens the surface.
- `lodestone:plugin@0.27.0` is the current main WIT world ABI; `lodestone:version-broker@0.1` is the broker ABI recorded in the lock.

## Dependencies

- `lodestone-wasm-host` owns the WIT import, capability gate, manifest lock, and load-time validation.
- `lodestone-registry` resolves the selected family and adapter without exposing version crates through the WASM boundary.
- `lodestone-model::VersionAdapter` supplies the small copied values used by the shell provider.
- The example guest at `crates/plugins/lodestone-chat-responder-wasm` exercises descriptor and lookup calls from a separately compiled WASM module.
