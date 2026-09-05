# Packet and action wiring: routers, gates, and cancellation

## What it is

How a decoded packet reaches a real consumer instead of an island, on both the serverbound
(hosting) and clientbound (joining) sides, and the two plugin-facing hooks — `EgressFilters`
and `ActionVetoes` — that let a plugin inspect, replace, suppress, or veto an action before it
takes effect or reaches the wire.

## How it works

### Serverbound: decode is not the bar, construction is

`ServerBound` (the hosting-side action enum) is declared in `crates/lodestone-server/src/
protocol.rs`; the arms that construct it live in `crates/versions/26.2/src/server_protocol.rs`
— a different crate, with nothing in the type system joining them. A variant can be declared,
matched by `dispatch_play_packet`, given a fully-written consumer, and covered by an
end-to-end test, and still be **constructed by no decode arm at all** — silently discarding
the packet forever while everything else stays green. This has happened more than once from a
single commit that wired consumers but updated only some of the matching decode arms.

`crates/versions/26.2/tests/serverbound_wiring.rs` closes this structurally: every variant
declared on `ServerBound` must be constructed somewhere in `server_protocol.rs`'s non-test
code, with comments and `#[cfg(test)]` blocks stripped before scanning (a stray comment or a
test assertion both look like a construction to a naive scanner, and both have hidden a real
island in this gate's own early drafts). It is lifetime-aware by lookahead rather than a
toggle — a `'` opening a lifetime and never closing has silently disabled comment-detection in
every other hand-rolled scanner in this repo.

**This gate and `cargo xtask connectedness` are complements, not substitutes.** `connectedness`
answers "does a decoded clientbound packet reach anything" and is blind to a missing
*constructor*; `serverbound_wiring.rs` answers "is every declared `ServerBound` variant ever
constructed" and is blind to a missing *consumer* — a variant that decodes and lands in
`dispatch_play_packet`'s no-op group is invisible to it. Use both, and remember `connectedness`
cannot see a canonicalisation defect either (see `docs/multi-protocol-seam.md`): it is silent
about *what value* flows through an already-connected wire.

**To find a packet that decodes but reaches nothing**, check three things in order: (1) does a
decode arm in the relevant family's adapter/`server_protocol.rs` actually construct the event
or action (not just parse and discard the bytes — a `let _ = …` in a decoder is a real,
distinct defect species: the bytes are consumed correctly and the value is dropped on the
floor); (2) is the variant claimed by a router (see below) or, on the hosting side, matched by
`dispatch_play_packet` into a real consumer; (3) for a packet carrying a discriminant/enum
field (`PLAYER_ACTION`, `PLAYER_COMMAND`, `INTERACT`, `CUSTOM_CLICK_ACTION`), check every
*ordinal* individually — a packet id can read as fully connected while one ordinal inside it
still falls through to `Ignored`.

### Teleport acknowledgement gate

`ACCEPT_TELEPORTATION` is a Play-only VarInt. `V770ServerProtocol` preserves that literal id in
`ServerBound::TeleportationAccepted`; `dispatch_play_packet` consumes it before the ordinary
match table through `TeleportAcknowledgements::accepts`. Only the current correction id clears
the pending gate, which makes the next movement packet observable; stale or duplicate ids leave
movement blocked. The later empty match arm is exhaustiveness only, not the consumer.

The serverbound connectedness scan recognizes this guarded early-return form as well as normal
match arms. If another packet must short-circuit before the main dispatcher, keep its body
non-empty, add a literal wrong-state and malformed-payload decode control, and add a scanner
fixture that distinguishes it from an empty exhaustiveness arm.

### Client tick boundaries and launch momentum

The 26.2 play-only `client_tick_end` frame has an empty body, but it is not a
discardable keep-alive. `V770ServerProtocol` lowers only an exactly empty frame
to `ServerBound::ClientTickEnded`; trailing bytes remain `Ignored`. The play
dispatcher uses that boundary to expire a connection's previous movement sample
when no player-position packet arrived in the tick. Projectile launches inherit
the sample's horizontal velocity and inherit its vertical velocity only while
the source is airborne. This prevents an idle player from throwing a projectile
with momentum left over from an earlier movement tick.

### Operator block-entity queries

The 26.2 `BLOCK_ENTITY_TAG_QUERY` decoder preserves the transaction id and packed
position. `dispatch_play_packet` requires command permission level 2, reads the
current dimension's `BlockEntityHandle`, and replies through
`ServerProtocol::encode_tag_query`. Missing entities produce an explicit null NBT
response; unauthorized requests receive no response. The reply echoes the id so
the client's pending inspection request can complete.

Serialization reuses `chunk_nbt::block_entity_to_nbt`, removing the persistence
metadata `id`, `x`, `y`, `z`, and `keepPacked` from the copied compound. Other fields, including
opaque block-entity data, survive. Extending a simulated block entity's save
representation therefore also extends its inspection response. Queries only see
entities present in the live registry; they do not load chunks from disk.
Other hosting families retain the trait's unsupported default until they provide
a response encoder. No new configuration or external dependency is required.

### Operator entity queries

On native hosts, `ENTITY_TAG_QUERY` uses the same permission level and response
encoder. The entity id is resolved to a live snapshot UUID, then matched to the
save record under one `MobHandle` lock, so the returned health, position, motion,
and dropped-item contents belong to the requested entity. Only the top-level
entity-type `id` is removed; a dropped stack's nested item `id` must survive.
Unknown ids receive no response, unlike an absent block entity's null response.

This path inherits `MobSim::saved_entities` and `SavedEntity::to_nbt` coverage:
simulated mobs and dropped items on native hosts. Players, vehicles, projectiles,
and browser-hosted entities remain unsupported, and unmodeled save fields remain
absent. Extending those requires an authoritative per-entity record source;
moving that source out of the filesystem-backed persistence module is the
prerequisite for browser support. Queries currently inspect snapshot/save lists
under the simulation lock, so repeated operator inspection scales with the live
population. They perform no disk access or world mutation.

### Clientbound: `lodestone_model::event::route`, an exhaustive table

The clientbound mirror of the serverbound problem above has its own document,
[`docs/event-routing.md`](./event-routing.md) — kept separate because
`crates/lodestone-model/src/event.rs` `include_str!`s it in a test that checks the doc's stated
island fraction against the real source, so that file must survive under its exact name. In
short: `ClientEvent` is `#[non_exhaustive]`, so an exhaustive `route(event) -> Route` table
lives beside the enum (the one place that can write one) and a new variant is a compile error
until it gets an arm, closing the same "compiles, tests green, reaches nothing" hole this
serverbound gate closes from the other side. `ClientAction` (serverbound, outbound from us) has
the mirror problem with no equivalent exhaustive table — check for a real producer by hand.

### Raw inbound packet observation

`RawPacketBusPlugin` installs the opt-in `Messages<RawPacket>` bus in the
caller's ECS world. The connection driver publishes `RawPacket { state,
packet_id, payload }` immediately after framing and before it calls the version
adapter, so a `MessageReader<RawPacket>` can inspect an undecoded packet without
taking a version-crate dependency. The payload excludes the packet id and outer
length framing and is copied only when the marker resource is present.

This surface is observation-only. A reader cannot replace the bytes, cancel the
packet, or inject another packet; the adapter remains the sole decoder and
directive producer. The bus is separate from `GameEventBusPlugin`, so a plugin
that needs only decoded events does not pay to copy every inbound payload.
Messages age at `TickSet::Send`; a reader that runs in another schedule must
choose its own hand-off or accept that the normal tick is the retention boundary.

### The outbound action hook: `EgressFilters`

`EgressFilters` (`crates/lodestone-ecs/src/egress.rs`) is where a plugin inspects, replaces, or
suppresses a `ClientAction` another plugin (or the local player) queued, after the tick has
finished and before it reaches the socket — ProtocolLib's outbound side, at the one layer
where it is version-free. A registered callback receives only `&ClientAction` and returns a
`Verdict`: `Allow`, `Suppress`, or `Replace(Box<ClientAction>)`. Filters run in priority order;
the **first non-`Allow` verdict wins** — a suppression cannot be reversed and a replacement is
not re-offered to later filters, so two filters cannot loop rewriting each other's output.

The callback is never handed the `World` — the drain runs while the driver holds the world
write guard, so a callback that could reach the `World` would be one `hold_read` away from a
reentrant deadlock. A filter needing world state keeps its own `Arc` refreshed by a system.
Scope is deliberately narrow: this hook sees only `ClientAction` (version-free) and must never
be extended to mutate encoded bytes, which would reopen a separate, still-open version-leak
concern at the adapter layer.

`ActionQueue` is the one sanctioned egress for plugin-originated actions, but it is not the
only path to the socket: several user-input paths (attack, use-item, container clicks,
sign/menu submission, respawn) call `send_action` directly and bypass this hook entirely,
because those verbs control their own wire ordering. A gate
(`egress_hook_coverage.rs`) enumerates the direct-send call sites so that list cannot silently
grow unnoticed; treat a new addition to it as a gap to close (route through `ActionQueue`)
rather than a line to append without comment. The hook also covers only the **outbound**
direction — there is no equivalent inbound hook for a plugin command arriving over the wire.

### Cancelable interaction verbs: `ActionVetoes`

`ActionVetoes` (`crates/lodestone-ecs/src/veto.rs`) is the veto point for interaction verbs a
protection or anti-grief plugin needs to cancel *before* they commit — `BlockBreak`,
`BlockPlace`, `EntityDamage`, `InventoryClick`, `PlayerMove`, and `PlayerInteract` are all wired.
A plugin
registers a predicate per verb, keyed by priority; the first `Deny` short-circuits, and later
predicates cannot un-deny. The predicate receives only a typed `VerbContext`, never the
`World`, for the same reentrancy reason `EgressFilters`' callback does not get one — the verb's
commit site is often a plain method already holding a guard.

`PlayerInteract` is asked exactly once at the start of `Sim::use_item_live`, after choosing the
same entity-first, block-second, air-last target that the click will commit. Its context carries
either the protocol entity id, the clicked `BlockPos`, or neither for air. A denial returns before
held-item use state, firework boost state, armour or block prediction, the shared use-sequence
counter, swings, sounds, and direct socket sends can change. The chosen ray targets are reused by
the allowed path, so a predicate cannot approve one target while the click commits another.

`InventoryClick` is asked inside `SharedState::menu_click` while its existing world write guard
is held, before `SessionMenus::click_action` runs. The context carries the active window id plus
the raw slot and button. A denial is a successful no-op at `ClientHandle::menu_click`: it leaves
the predicted slots, cursor, drag state, and menu state id untouched, does not wake read-model
waiters, and never places a `ClientAction` on the driver's channel.

This is a different layer from `EgressFilters`, not a redundant one: `ActionVetoes` stops a
verb before its effect is computed at all, so client state never diverges; `EgressFilters`
inspects an already-decided `ClientAction` after the fact, at the queue drain. `EgressFilters`
structurally cannot cover attack, use-item, or inventory click (they bypass `ActionQueue`
entirely), which is exactly why the veto exists as a separate, verb-keyed mechanism rather than
a special case of the egress hook.

## How to change it, and the gotchas

- **Adding a `ServerBound` variant**: `serverbound_wiring.rs` fails until a decode arm
  constructs it — fix the arm, never add an exemption. A new decode arm that lifts a packet
  out of `Ignored` needs edits in three different crates: the variant
  (`lodestone-server::protocol`), the decode arm (`v26-2::server_protocol`), and a
  `dispatch_play_packet` arm plus consumer (`lodestone-server::server`); missing the third
  leaves it stranded per `connectedness`, missing the second leaves the consumer dead per
  `serverbound_wiring.rs`.
- **Adding a `ClientEvent` variant**: write the `route()` arm and update the island count in
  `docs/event-routing.md` in the same commit — see that doc for both gates.
- **Observing raw inbound packets**: add `RawPacketBusPlugin` to the caller's
  ECS `App` and read `RawPacket` with `MessageReader<RawPacket>`. Do not add
  mutation or cancellation to this bus; those operations belong to a
  version-typed adapter decorator and are intentionally outside the shared
  plugin surface.
- **Expected values for any wiring gate must come from outside the code under test** — a
  decompiled `STREAM_CODEC`, a registry report, or captured bytes, never `decode(encode(x))
  == x` against our own encoder.
- **Never hand an `EgressFilters` callback or an `ActionVetoes` predicate a `World`, an
  `EcsHandle`, or anything reaching either.** The soundness argument for both hooks is that
  they have no way to re-enter the lock; an overload "just for one case" deletes the argument.
- **Ask a veto before the predictor runs, never after** — a verb that advances a
  block-prediction sequence must be asked ahead of that, since a denial cannot un-take the
  sequence number.
- A source-scanning gate (serverbound wiring, egress coverage, veto coverage) cannot tell
  whether an ask or a construction is in the *right place*, only that one exists somewhere in
  scope — pair it with a runtime assertion for placement-sensitive claims.

## Configuration

- `EgressFilterPlugin` and `ActionVetoPlugin` are both opt-in bevy plugins; a client with
  neither installed pays only a `get_resource`/bitset-test lookup per tick/ask.
- `RawPacketBusPlugin` is independently opt-in. Without it, the client does not
  clone inbound payloads or write to an ECS message queue.
- No environment variables gate any of this.

## Dependencies

- `lodestone-server/src/protocol.rs` (`ServerBound`, `ServerProtocol`) and
  `protocol/v26-2/src/server_protocol.rs` (the decode arms) for serverbound wiring.
- `docs/event-routing.md` and `lodestone-model/src/event.rs` for clientbound routing.
- `lodestone-ecs/src/egress.rs` and `veto.rs`, both depending only on `bevy_app`/`bevy_ecs`/
  `lodestone-model`; ask/drain call sites live in `lodestone-client`, `lodestone-shell`, and
  `lodestone-controller`.
- `lodestone-ecs/src/events.rs` (`RawPacket`, `RawPacketBusPlugin`) and
  `lodestone-client/src/driver.rs`/`state.rs` for the pre-decode publication
  point and the opt-in cache.
- `cargo xtask connectedness` for the decoded/connected measurement this doc's gates
  complement.
