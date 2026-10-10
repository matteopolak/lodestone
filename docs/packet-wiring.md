# Packet and action wiring: routers, gates, and cancellation

## What it is

How a decoded packet reaches a real consumer instead of an island on both the serverbound (hosting) and clientbound (joining) sides, plus the two plugin hooks, `EgressFilters` and `ActionVetoes`, that let a plugin inspect, replace, suppress or veto an action before it takes effect or reaches the wire.

## How it works

### Serverbound: construction is the bar, not decode

`ServerBound` is declared in `crates/lodestone-server/src/protocol.rs`; the arms that construct it live in `crates/versions/26.2/src/server_protocol.rs`, a different crate with nothing in the type system joining them. A variant can be declared, matched by `dispatch_play_packet`, given a consumer and an end-to-end test and still be constructed by no decode arm, silently discarding the packet.

`crates/versions/26.2/tests/serverbound_wiring.rs` closes this: every `ServerBound` variant must be constructed in `server_protocol.rs`'s non-test code, with comments and `#[cfg(test)]` stripped first (a stray comment or test assertion looks like a construction to a naive scanner). It is lifetime-aware by lookahead: a `'` opening a lifetime and never closing has disabled comment detection in other hand-rolled scanners here.

This gate and `cargo xtask connectedness` are complements. `connectedness` asks whether a decoded clientbound packet reaches anything and is blind to a missing constructor; `serverbound_wiring.rs` asks whether every variant is ever constructed and is blind to a missing consumer (a variant landing in `dispatch_play_packet`'s no-op group). Neither sees a canonicalisation defect (see [multi-protocol seam](multi-protocol-seam.md)).

`connectedness` takes each family's packet-id denominators from its own `packet_ids.rs`. A family with no adapter of its own (`26.3`) is scanned through its base family's adapter and `ServerProtocol` (`SHARED_LOGIC_BASES` in `xtask/src/connectedness.rs`), including release arms keyed by packet name (`Some("minecraft:...")`) and renamed serverbound packets (`SERVERBOUND_RENAMES`). A new dialect-only family needs an entry in both tables.

To find a packet that decodes but reaches nothing, check in order: (1) the decode arm constructs the event or action (a `let _ = ...` in a decoder consumes bytes and drops the value); (2) a router claims it, or `dispatch_play_packet` matches it into a real consumer; (3) for packets with a discriminant (`PLAYER_ACTION`, `PLAYER_COMMAND`, `INTERACT`, `CUSTOM_CLICK_ACTION`), check each ordinal, since one can still fall to `Ignored`.

### Teleport acknowledgement gate

`ACCEPT_TELEPORTATION` is a Play-only VarInt that `V770ServerProtocol` keeps as `ServerBound::TeleportationAccepted`. `dispatch_play_packet` consumes it before the main match via `TeleportAcknowledgements::accepts`; only the current correction id clears the gate that makes the next movement packet observable, and stale or duplicate ids leave movement blocked. The later empty match arm is exhaustiveness only. The connectedness scan recognises this guarded early-return form; a new short-circuit packet needs a non-empty body, a wrong-state and malformed-payload decode control, and a scanner fixture distinguishing it from an empty arm.

### Client tick end

The play-only `client_tick_end` frame is empty but not a keep-alive. Only an exactly empty frame becomes `ServerBound::ClientTickEnded` (trailing bytes stay `Ignored`). The dispatcher uses it to expire a connection's previous movement sample when no position packet arrived that tick; projectile launches inherit the sample's horizontal velocity, and vertical only while airborne, so an idle player cannot throw with stale momentum.

### Operator tag queries

`BLOCK_ENTITY_TAG_QUERY` keeps the transaction id and packed position. `dispatch_play_packet` requires permission level 2, reads the current dimension's `BlockEntityHandle` and replies through `ServerProtocol::encode_tag_query` echoing the id; a missing entity gets an explicit null NBT, an unauthorised request gets nothing. Serialisation reuses `chunk_nbt::block_entity_to_nbt` minus the persistence metadata (`id`, `x`, `y`, `z`, `keepPacked`), so extending a block entity's save form extends the response. Only live-registry entities are seen (no disk loads); other families keep the unsupported default.

Chunk packets use a separate `chunk_nbt::block_entity_update_nbt`: the packet already carries id and position, save metadata and spawner potentials are omitted, and an empty update compound is written as the network `TAG_End` null tag (furnaces, generated chests, beehives). Data-bearing spawners and End gateways keep their update fields.

`ENTITY_TAG_QUERY` (native hosts) shares the level and encoder. The entity id resolves to a snapshot UUID and then to the save record under one `MobHandle` lock, so health, position, motion and dropped-item contents belong to the requested entity. Only the top-level type `id` is removed (a dropped stack's item `id` survives). Unknown ids get no response. Coverage is `MobSim::saved_entities`/`SavedEntity::to_nbt` (mobs and dropped items); players, vehicles, projectiles and browser hosts are unsupported until an authoritative per-entity record source moves out of the filesystem persistence module. Queries scan snapshot and save lists under the sim lock and touch no disk.

### Clientbound routing

`ClientEvent` is `#[non_exhaustive]`, so an exhaustive `route(event) -> Route` table beside the enum makes a new variant a compile error until it has an arm. It has its own doc, [event routing](event-routing.md), which `crates/lodestone-model/src/event.rs` `include_str!`s in a test checking the stated island fraction, so that file must keep its exact name. `ClientAction` has no exhaustive table; check for a real producer by hand.

### Raw packet observation

`RawPacketBusPlugin` installs an opt-in `Messages<RawPacket>` bus. The connection driver publishes `RawPacket { state, packet_id, payload }` after framing and before the adapter, so a `MessageReader<RawPacket>` sees undecoded packets without a version-crate dependency. `OutboundRawPacketBusPlugin` publishes `OutboundRawPacket` after adapter and decorator encoding and before transport framing. Payloads exclude the id and length framing.

Both are observation-only: a reader cannot replace, cancel or inject. The outbound bus is bounded by per-tick packet and byte limits; when full it drops only the observer copy, counts it, and still writes the packet. They are separate from `GameEventBusPlugin`. Messages age at `TickSet::Send`; a reader in another schedule must hand off itself.

### Outbound hook: `EgressFilters`

`EgressFilters` (`crates/lodestone-ecs/src/egress.rs`) lets a plugin inspect, replace or suppress a queued `ClientAction` after the tick and before the socket, at the one version-free layer. A callback gets only `&ClientAction` and returns `Verdict::{Allow, Suppress, Replace(Box<ClientAction>)}`. Filters run in priority order and the first non-`Allow` wins; a replacement is not re-offered, so filters cannot loop.

Callbacks never get the `World`: the drain runs under the world write guard, so access is one `hold_read` from a reentrant deadlock. A filter needing world state keeps its own `Arc` refreshed by a system. The hook must never mutate encoded bytes (it would reopen the version-leak concern at the adapter). `ActionQueue` is the sanctioned egress but not the only path: attack, use-item, container clicks, sign and menu submission and respawn call `send_action` directly, bypassing the hook. `egress_hook_coverage.rs` enumerates those sites; treat a new entry as a gap to close, not a line to append. There is no inbound equivalent.

### Cancelable verbs: `ActionVetoes`

`ActionVetoes` (`crates/lodestone-ecs/src/veto.rs`) cancels `BlockBreak`, `BlockPlace`, `EntityDamage`, `InventoryClick`, `PlayerMove` and `PlayerInteract` before they commit. Plugins register predicates per verb by priority; the first `Deny` short-circuits. Predicates get a typed `VerbContext`, never the `World`, for the same reentrancy reason.

`PlayerInteract` is asked once at the start of `Sim::use_item_live`, after choosing the entity-first, block-second, air-last target the click will commit; the context carries the entity id, clicked `BlockPos`, or neither. A denial returns before held-item state, firework boost, armour or block prediction, the use-sequence counter, swings, sounds and direct sends change, and the allowed path reuses the same targets. `InventoryClick` is asked in `SharedState::menu_click` under its existing write guard before `SessionMenus::click_action`, with window id, raw slot and button; a denial is a successful no-op at `ClientHandle::menu_click` leaving slots, cursor, drag and state id untouched, waking no waiter and sending no action.

Vetoes stop a verb before its effect is computed, so client state never diverges; egress filters see an already-decided action. Egress structurally cannot cover attack, use-item or inventory click, which is why the veto is a separate verb-keyed mechanism.

## How to change it

- **New `ServerBound` variant**: `serverbound_wiring.rs` fails until a decode arm constructs it; never add an exemption. Lifting a packet out of `Ignored` touches three crates: the variant (`lodestone-server::protocol`), the decode arm (`v26-2::server_protocol`) and a `dispatch_play_packet` arm plus consumer. Missing the third strands it per `connectedness`, which scans `server.rs` and every non-test file under `server/`; missing the second kills the consumer per the wiring gate.
- **New `ClientEvent` variant**: write the `route()` arm and update the island count in `event-routing.md` in the same commit.
- **Raw observation**: add the bus plugin and read with `MessageReader`. Mutation and cancellation belong to a version-typed adapter decorator, not here. Set explicit limits for bursty outbound readers; overflow drops the observation, never the packet. `OutboundPacketLoggerPlugin` is the example.
- Expected values for any wiring gate must come from outside the code under test (captured bytes, registry report), never `decode(encode(x)) == x`.
- Never hand an egress callback or veto predicate a `World`, `EcsHandle` or anything reaching either; the soundness argument is that they cannot re-enter the lock.
- Ask a veto before the predictor runs: a denial cannot un-take a prediction sequence number.
- Source-scanning gates prove an ask or construction exists in scope, not that it is in the right place; pair with a runtime assertion for placement.

## Configuration

`EgressFilterPlugin`, `ActionVetoPlugin`, `RawPacketBusPlugin` and `OutboundRawPacketBusPlugin` are each opt-in; absent, the client pays only a resource or bitset lookup per tick or ask and clones no payloads. The outbound bus defaults to 256 packets and 1 MiB per tick. No env vars.

## Dependencies

- `lodestone-server/src/protocol.rs` and the 26.2 family's `server_protocol.rs`.
- `lodestone-model/src/event.rs` and [event routing](event-routing.md).
- `lodestone-ecs/src/{egress,veto,events}.rs` (only `bevy_app`, `bevy_ecs`, `lodestone-model`); call sites in `lodestone-client` (`driver.rs`, `state.rs`), `lodestone-shell` and `lodestone-controller`.
- `crates/plugins/lodestone-event-logger`'s `OutboundPacketLoggerPlugin` (drains at `EventPriority::Monitor`).
- `cargo xtask connectedness`.
