# Event routing: making a mis-routed `ClientEvent` a compile error

## What it is

`lodestone_model::event::route` is a single exhaustive table saying which of the client's routers claim each `ClientEvent` variant, so that adding a variant and forgetting to wire it is a compile error rather than a silent nothing (the "island" defect class).

## How it works

### Module layout

The public `lodestone_model::event::*` surface is re-exported from the small `event.rs` facade. Payloads are grouped in `event/chat.rs` (chat, signing, identity), `event/entity.rs` (movement, metadata, equipment, player lists), `event/world.rs` (dimensions, block-state provenance, particles), `event/scoreboard.rs` (sounds, scoreboard, teams, boss bars) and `event/session.rs` (recipe, map, advancement, server links). `event/client.rs` owns the `ClientEvent` enum; `event/routing.rs` owns `Route`, `route` and their tests. Moving a definition between domains does not change downstream paths.

### The table

`ClientEvent` is deliberately not `#[non_exhaustive]`, so a consumer can match it without a wildcard and a new variant is a compile error there. `route(event: &ClientEvent) -> Route` is one exhaustive match (no wildcard) in `routing.rs`:

```rust
pub struct Route {
    pub ingest: bool,            // lodestone_ecs::ingest  - per-entity ECS state
    pub session: bool,           // lodestone_ecs::session - local-player scalars
    pub shell: bool,             // lodestone_shell::net::forward - block/world state
    pub shell_conditional: bool, // that shell arm is guarded
    pub client: bool,            // consumed inside lodestone-client, not by a router
}
```

Flags are not exclusive (`Login` writes an ECS entity component, a local-player scalar, a shell `NetUpdate` and a client-only latch). Ownership convention: per-entity state is `ingest`, local-player scalars `session`, block and world state `shell` (via the shell's `NetUpdate` stream, no `handles_event` arm). Ask what a fold writes, not what the packet is called: a debug feed keyed by subscription is `session` though it names an entity (it outlives the entity row); a fold writing a vehicle's own position is `ingest` though the packet has no entity id.

`net::forward` is exhaustive: variants with no consumer are named in one explicit ignore arm that carries `debug_assert!(!route(&event).must_forward(), ...)`, where `must_forward()` is `shell && !shell_conditional`, so a shell-routed variant listed there fails every debug test. Two guarded arms (a literal block-break sub-event id, a lightning-only spawn filter) are `shell_conditional`.

`Route::NOWHERE` is legal for an event with no consumer yet but must be typed deliberately: `route_tests::route_has_no_catch_all_arm` refuses a `_ => Route::NOWHERE` rewrite. Flipping a flag is not wiring: a router that is asked but has no system still drops silently, so write the system and the flag together. `ingest::tests::handles_event_covers_exactly_the_variants_with_a_system` feeds one instance of every claimed variant through the real schedule as the runtime half.

### The island count

**0 of 142** variants are currently `Route::NOWHERE`. `lodestone_model::event::event_tests::the_island_count_in_the_docs_matches_this_source` derives both numbers from `routing.rs` (denominator from the `ClientEvent` variants, numerator from arms whose right-hand side is exactly `Route::NOWHERE`, excluding `..Route::NOWHERE` spreads) and fails if this line drifts. It reads this file by `include_str!` and the line must start with `**N of M**` followed by `variants are currently` and `Route::NOWHERE` in backticks. Update the number and the source together.

Routing establishes a consumer boundary; connectedness checks and schedule tests verify it reaches a real consumer. Notes on consumers whose route is not obvious:

- `PlayerCombatEntered`/`Ended` fold into one local combat-session component (the F3 HUD shows `Combat: active` or `Combat: ended (N ticks)`, no client timer).
- `MountScreenOpened` folds into the menu session directly (no `ScreenOpened` companion); the mount's name and preview are unavailable at the version-free menu boundary, so the title is blank.
- `ServerDataReceived` folds the styled MOTD and favicon into session state (F3 shows a one-line projection). `SimulationDistanceChanged` folds into a session component shown as `SD` on the F3 entity line only after it arrives, distinct from the streamed-view radius. `ItemCooldown` folds into session state and the app projects the remaining fraction onto matching hotbar slots (no second clock).
- `SoundStopped` crosses to the audio boundary, which keeps handles only for packet-created voices, so name and category filters never cancel predicted sounds or ambience.
- `ProjectilePowerChanged` goes `ingest` to an entity component to the render track's integrated projectile state (next tick adds the power along velocity, applies inertia; zero stops only the gain).
- `ChunkCacheCenterChanged` goes `net::forward` to `Sim::poll_net`, which records the streamed-view centre for the loading grid. `PlayerLookAt` is resolved in `net::forward` and writes the existing `PhysicsState` pose (camera, ray, listener and outgoing movement all read it). `CameraSet` retains only the selected entity id and `Sim::render_camera` resolves it each frame (the local-player id restores the normal camera).
- Claimed by `Route::client` because `lodestone_client::driver::Driver::emit` answers before the shell loop: clientbound ping, pushed resource pack, deleted chat (removes the signature from the pending acknowledgement tracker), cookie request and store (in-memory store, response action), transfer request (`SessionOutcome::Transferred`; the shell records the target for the disconnect message), and `CustomPayload` (`SharedState::apply` publishes it on the optional `GameEvent` bus; the app installs a typed `minecraft:brand` consumer). A resource-pack pop is a shell interception (the connection loop clears the active pack and prompt before generic forwarding). A play-state pong is claimed by the client read model (the F3 ping probe compares it to the portable clock). Routing-table silence is not proof of no consumer when a reply is synthesised upstream.
- A few variants stay `Route::NOWHERE` on purpose as negative controls (a world-state scalar beside several wired siblings, so an over-broad fold is caught); a gate asserts `route(&that_variant).is_island()` before relying on it.

### What it does not do

- It does not check a claimed router has a system (`handles_event`'s coverage test does).
- It does not cover version adapters: chunk payloads reach the world through `lodestone_world::WorldSink` and never become routed events (`ChunkLoaded` and `ChunkLightChanged` are post-write signals; `ChunkUnloaded` is client-only).
- It does not cover the serverbound direction; `ClientAction` has no table (see [packet wiring](packet-wiring.md)).
- It is not `cargo xtask connectedness`, which measures clientbound decode to event wiring, a different axis.

## How to change it

- Adding a variant: write the arm; `cargo check -p lodestone-model` refuses until you do. Do not take the compiler's `_ => todo!()` / `_ => Route::NOWHERE` suggestion.
- Changing an existing route changes runtime behaviour: do it in its own commit.
- Update the island count here in the same commit as any `route()` change.

## Configuration

None.

## Dependencies

- `crates/lodestone-model/src/event/{client,routing}.rs` and the `event.rs` facade. `routing.rs` `include_str!`s this file (and `client.rs`), so these paths and the `**N of M** variants are currently` phrasing are load-bearing.
- `crates/lodestone-ecs/src/{ingest,session}.rs` (`handles_event` derives from `route(e).ingest`/`.session`), `crates/lodestone-shell/src/net.rs` (the `debug_assert!`), `crates/lodestone-client/src/{driver,state}.rs`.

The layering is inverted on purpose: the leaf model crate names its consumers by crate path, the cost of the one property nothing else buys, a compile error.
