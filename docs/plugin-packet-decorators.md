# Packet decorators: the version-locked ProtocolLib-class escape hatch

## What it is

A wrapper struct implementing `ServerProtocol` (server) or `VersionAdapter` (client) around a concrete family's own type, forwarding most calls and intercepting the ones a plugin cares about. It is the one route to ProtocolLib-class access (see, drop, rewrite or append traffic in either direction), at the cost of depending on a concrete version crate instead of the version-free shared crates every other plugin surface targets. Executable proof, one test per verb per direction with an undecorated control first: `crates/versions/26.2/tests/server/server_protocol_decorator_escape_hatch.rs` and `crates/versions/26.2/tests/singleplayer_lan/client_adapter_decorator_escape_hatch.rs`.

## How it works

Both traits are object-safe (no generic methods, no `Self` in argument position), so the ordinary decorator shape works: hold the wrapped value plus hook state, implement the trait, call through and modify the request or result. `impl<P: ServerProtocol + ?Sized> ServerProtocol for Box<P>` is the in-tree example.

### The forwarding hazard

Each trait has only seven methods without a default body (`ServerProtocol`: `decode`, `login_success`, `begin_configuration`, `begin_play`, `begin_chunk_batch`, `encode_chunk`, `end_chunk_batch`; `VersionAdapter`: `protocol_version`, `minecraft_versions`, `supports`, `begin_login`, `handle_packet`, `encode_action`, `build_encryption_response`). Every other method (`encode_system_chat`, `welcome_message`, `encode_teleport`, `entity_dimensions`, `block_hardness`, dozens more) defaults to `ServerDirective::None` or an empty value. **A decorator that does not forward one silently answers with the default instead of the wrapped behaviour**: a decorator forwarding only the required seven joins a client but every optional packet family (keep-alives, boss bars, attributes, welcome message) stops firing with no error. Forward every method you are not hooking, one line each.

### Verbs by direction

| trait | inbound | outbound |
|---|---|---|
| `ServerProtocol` | `decode(..) -> ServerBound` (one value) | `welcome_message(..) -> Vec<ServerDirective>` and siblings (batch); `encode_system_chat(..) -> ServerDirective` (one value) |
| `VersionAdapter` | `handle_packet(..) -> Result<Vec<Directive>, _>` (batch) | `encode_action(..) -> Result<Option<(i32, Vec<u8>)>, _>` (at most one) |

Drop and rewrite work in both directions: `decode` can turn `ServerBound::Chat` into `ServerBound::Ignored` or rewrite its `message`; `encode_action` can return `Ok(None)` or delegate with a rewritten `ClientAction`. **Append works only where a method returns a batch**: `welcome_message` can call the wrapped one and push an extra `ServerDirective::Send` built from the same protocol's `encode_system_chat`; `handle_packet` can push an extra `Directive::Emit(..)`. `decode` and `encode_action` return exactly one value, and `ServerBound` and `ClientAction` are closed enums a decorator's crate cannot extend, so an inbound decorator can see an unknown packet's bytes but cannot inject a new kind of action.

A `ServerProtocol` decorator sees both directions of the connection at the one seam `IntegratedServer` calls through; a `VersionAdapter` decorator is the client mirror. Passed to `ClientBuilder::new` as a boxed trait object, it gives a headless bot ProtocolLib-class visibility with no server change.

### The explicit native version lock

`lodestone_registry::plugin::VersionDescriptor` (family label, negotiated protocol, native ABI) is the identity a version-specific native plugin carries beside its decorator. The host's selected descriptor must pass `VersionDescriptor::validate` before load; equality is exact and a failure reports both sides. This is separate from the wrapper: the wrapper supplies behaviour, the descriptor stops it attaching to a different wire or registry shape. It is unstable and native-only, grants no raw pointers, ECS, sockets or host calls, and has no windowed-shell registration path yet. A host with an `App` registration seam implements `VersionLockedPlugin` and calls `register_version_locked_plugin`, which validates first and invokes the callback only on success (controls in `crates/lodestone-registry/tests/native_plugin_registration.rs`, against `lodestone_app::client_app()`, including a mismatch that never runs the build callback).

## How to change it, and the gotchas

- **Version-locked.** A hook written against one family's behaviour does not transfer: `V770ServerProtocol` sends chat through `encode_system_chat` and leaves `welcome_message` empty, while older families may use that hook for join content or shape `ServerBound::Chat` differently (no `salt`/`signature` before 1.19). A decorator compiles against any family but its hooks may silently match nothing. Re-verify against the target family's `server_protocol.rs`/`adapter.rs` before porting.
- Test more than the required seven methods, or optional packet families will be silently disabled unnoticed.
- **Unsandboxed.** Nothing validates a rewritten or appended packet's bytes; a `ServerDirective::Send` with a nonsense `packet_id`/`payload` goes straight to the wire. The shared surface (`ActionVetoes`/`EgressFilters`, [packet wiring](./packet-wiring.md)) never hands plugins raw bytes for this reason.
- Reentrancy rules elsewhere do not relax: decorator methods run on the same connection task as the wrapped ones.

## Configuration

A plain Rust generic/trait-object wrapper: no manifest, feature or environment variable. The `Cargo.toml` edge onto the concrete version crate is the compile-time opt-in. A host-managed native plugin also declares a `VersionDescriptor` validated against the host's before construction.

## Dependencies

- Server: `lodestone-server` (`ServerProtocol`, `ServerDirective`, `ServerBound`, `IntegratedServer`) plus a version crate (e.g. `lodestone-v26-2`'s `V770ServerProtocol`).
- Client: `lodestone-client` (`ClientBuilder`), `lodestone-model` (`VersionAdapter`, `Directive`, `ClientAction`, `ClientEvent`) plus the version crate's adapter constructor (`lodestone_v26_2::adapter()`/`V770Adapter`).
- Load-time identity: `lodestone-registry::plugin::{VersionDescriptor, VersionCompatibilityError, VersionLockedPlugin, register_version_locked_plugin}` (a host-managed loader may depend on the registry for this without changing the decorator).

See also [packet wiring](./packet-wiring.md), [plugin API](./plugin-api.md), [plugin capability audit](./plugin-capability-audit.md).
