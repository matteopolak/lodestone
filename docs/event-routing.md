# Event routing — making a mis-routed `ClientEvent` a compile error

## What it is

`lodestone_model::event::route` is a single **exhaustive** table saying which of the
client's event routers claim each `ClientEvent` variant. It exists so that adding a
variant and forgetting to wire it is a **compile error** (`E0004`) rather than a
silent nothing.

That silence is this repo's dominant defect class — `CLAUDE.md` §1's *island*, with
nine confirmed instances. Four of them were the same mechanical failure in the same
two functions.

## Why the old shape could not work

`ClientEvent` is `#[non_exhaustive]`. That attribute means **no crate outside
`lodestone-model` can write an exhaustive match over it** — rustc requires a
wildcard arm, and there is nothing a downstream crate can do about it. So every
consumer ended in a terminal `_ =>`, and:

* `lodestone_ecs::ingest::handles_event` was a `matches!` — new variant ⇒ `false`.
* `lodestone_ecs::session::handles_event` was a `matches!` — new variant ⇒ `false`.
* `lodestone_shell::net::forward` ends in `_ => return Ok(())` — new variant ⇒ dropped.

A new variant therefore compiled with **zero** routing arms anywhere and reached
nothing. `SharedState::apply` (`crates/lodestone-client/src/state.rs`) unions the
first two predicates and otherwise falls through to a legacy echo that handles
`TeleportPlayer` and nothing else, so "not listed" and "deliberately unrouted" were
the same observation.

The attribute is *correct* — it keeps external plugin code from breaking on a new
variant — and it is not removed. Inside the defining crate it simply does not bind,
which is the whole trick: the table lives next to the enum, where the match can be
exhaustive.

## How it works

```rust
pub struct Route {
    pub ingest: bool,            // lodestone_ecs::ingest  — per-entity ECS state
    pub session: bool,           // lodestone_ecs::session — local-player scalars
    pub shell: bool,             // lodestone_shell::net::forward — block/world state
    pub shell_conditional: bool, // that shell arm is guarded (see below)
    pub client: bool,            // consumed inside lodestone-client, not by a router
}

pub fn route(event: &ClientEvent) -> Route { /* exhaustive match, no wildcard */ }
```

Three consumers, each earning its flag differently:

| flag | who enforces it | failure mode if it drifts |
|---|---|---|
| `ingest` | `ingest::handles_event` **is** `route(e).ingest` | none — it is a derivation |
| `session` | `session::handles_event` **is** `route(e).session` | none — it is a derivation |
| `shell` | a `debug_assert!` on `net::forward`'s catch-all | fires in every test and oracle run |
| `shell_conditional` | nothing; it keeps that assert correct | a guarded arm would trip the assert |
| `client` | nothing; documentation | `Route::NOWHERE` would over-report islands |

### Booleans, not an enum

The claims are **not exclusive**, and an enum would have forced a false choice that
loses information on day one:

* `Login` → `ingest` (entity id + `EntityIndex` entry) **and** `session` (game mode,
  dimension, alive) **and** `shell` (`NetUpdate::LoggedIn`) **and** `client` (the
  driver's `player_loaded` latch). Four disjoint writes off one event.
* `EntityPassengersChanged` → `ingest` (the `Passengers`/`Vehicle` component pair)
  **and** `session` (the local player's own `Riding` scalar).

Asserted, not asserted-in-prose, by
`event::route_tests::one_event_can_be_claimed_by_two_routers_at_once`.

### Why `forward` is not exhaustive

Making `net::forward` an exhaustive match would put a ~100-arm `match` into
`net.rs`, a permanently contended choke-point file. It keeps its catch-all and gains
one `debug_assert!(!route(&event).must_forward(), …)` instead — so a shell-routed
variant with no arm fails **loudly** in every debug test and oracle run rather than
quietly costing a chest lid.

`must_forward()` is `shell && !shell_conditional`. Two arms in `forward` are
*guarded* and so must be excluded, or the assert would fire constantly on correct
traffic:

* `LevelEvent` — the arm matches the literal sub-event `2001` (block-break effect).
* `EntitySpawned` — the arm is guarded on `lightning_bolt`, to count flashes.

Both are properties of `net.rs` today, not of the events. If either becomes
unconditional, clear the flag and the assert gets stricter for free.

## The convention

It lives as a comment **directly above the match**, because that is where the
decision is actually made:

* **per-entity state** → `ingest`
* **local-player scalars** → `session`
* **block and world state** → `shell` (it travels the shell's own `NetUpdate`
  stream and needs no `handles_event` arm at all — the chest-lid work needed none)

Guessing the `ingest`/`session` fork wrong has cost work twice:
`DimensionTypeChanged` and `AbilitiesChanged` were both briefed as `ingest` and both
belong to `session`. An arm in the wrong router compiles, unit-tests green, and
never runs.

## The trade, stated plainly

Adding a `ClientEvent` variant now costs **one mandatory one-line arm** in `route`,
and in exchange it **cannot** island silently. Before, it cost nothing and risked
silence. `Route::NOWHERE` is still a legal answer — but it has to be typed on
purpose, with a reason beside it, and that is the entire difference between a
decision and the defect.

## How to change it

**Adding a variant.** Write the arm. `cargo check -p lodestone-model` will refuse to
compile until you do:

```
error[E0004]: non-exhaustive patterns: `&ClientEvent::Foo { .. }` not covered
    --> crates/lodestone-model/src/event.rs:2087:11
help: ensure that all possible cases are being handled by adding a match arm with a
      wildcard pattern or an explicit pattern as shown
```

**Do not take rustc's advice.** The suggested `_ => todo!()` — and its friendlier
cousin `_ => Route::NOWHERE` — restore exactly the wildcard that
`#[non_exhaustive]` forces everywhere else, delete the guarantee in one line, and
leave a green tree behind. `route_tests::route_has_no_catch_all_arm` reads this
file's own source and fails if one appears; it carries its own positive control on
both spellings.

**Flipping a flag is not wiring.** The flag only decides who gets *asked*. A router
that is asked but has no system for the event drops it exactly as silently as an
unrouted one. Write the system, then the flag, in one commit. The runtime half of
the guarantee is `ingest::tests::handles_event_covers_exactly_the_variants_with_a_system`,
which feeds one of every claimed variant through the real schedule: the table proves
the decision was made, that test proves the system exists.

**Changing an existing route changes runtime behaviour.** Do it in its own
reviewable commit, not as a drive-by while landing something else.

## Islands: variants this table found reaching nothing

41 of 98 variants are `Route::NOWHERE`. Most are simply decoded ahead of a consumer,
which is a normal state for a from-scratch client. **Three are different — a fold
exists, is unit-tested, and nothing feeds it**, which is the island shape exactly:

| variant | the fold that never runs |
|---|---|
| `BlockDestruction` | `lodestone_game::mining::BlockCrackOverlay::apply` — other players' crack overlay. No caller outside its own file and tests. |
| `HeldSlotChanged` | `lodestone_game::player_state::HudState::apply` — server-driven hotbar selection (`/item`, creative). No caller outside its own file and tests. |
| `DifficultyChanged` | the same `HudState::apply`. |

`HudState` was superseded by the `lodestone-ecs` session components (see
`session.rs`'s note on `Vitals`: `HudState` has no "unreported" bit, so Stage 3 did
not adopt it), which is *why* it has no caller — but the two events it folds were
never re-homed onto a component, so they now reach nothing at all.

The remaining 38, listed for the record and not as a defect claim:

`Ping`, `SpawnPositionChanged`, `BlockChangedAck`, `ChunkCacheCenterChanged`,
`ChunkCacheRadiusChanged`, `SimulationDistanceChanged`, `EntityStatus`,
`EntityLeashed`, `VehicleMoved`, `ItemCooldown`, `PlayerRotationSet`, `CameraSet`,
`BookOpened`, `SoundStopped`, `TabListChanged`, the six `WorldBorder*`,
`PlayerCombatEntered`, `PlayerCombatEnded`, `SignEditorOpened`,
`AdvancementsTabSelected`, `ProjectilePowerChanged`, `MountScreenOpened`,
`GameRulesChanged`, `TransferRequested`, `CookieRequested`, `CookieStored`,
`ResourcePackPushed`, `ResourcePackPopped`, `CustomPayload`, `ServerDataReceived`,
`PongReceived`, `ChatMessageDeleted`, `PlayerLookAt`.

Two of those are worth a second look by whoever owns the relevant subsystem, and are
flagged rather than changed here:

* **`Ping`** is a clientbound ping the vanilla client answers with `pong`. Nothing
  consumes it and no `ClientAction` producer exists, which is the *outbound* island
  shape `ClientAction::SetFlying` had. `PongReceived` is likewise unconsumed.
* **`TabListChanged`** carries the tab list header and footer. `SessionTabList`
  folds the player rows but this event reaches nothing, so a server's header/footer
  cannot render.

## What this table does **not** do

* It does not measure whether a claimed router has a system. That is
  `handles_event`'s coverage test.
* It does not know about the version adapters. Chunk payloads reach the world
  through `lodestone_world::WorldSink` directly, so `ChunkLoaded`/`ChunkUnloaded`
  are marked `client` as signals; the heavy data never travels the event stream.
* It does not cover the **serverbound** direction. `ClientAction` has the mirror
  problem (`SetFlying` was encoded by four adapters with zero producers) and no
  equivalent table.
* It is not `cargo xtask connectedness`, which measures clientbound decode → event
  wiring and nothing else.

## Configuration

None. No features, no env vars.

## Dependencies

* `crates/lodestone-model/src/event.rs` — `Route`, `route`, and the table.
* `crates/lodestone-ecs/src/ingest.rs`, `session.rs` — `handles_event`, one line each.
* `crates/lodestone-shell/src/net.rs` — the `debug_assert!` in `forward`'s catch-all.
* `crates/lodestone-client/src/state.rs` — `SharedState::apply`, which unions the two
  predicates. Unchanged by this work; it now consults the table transitively.

The layering is deliberately inverted: the leaf model crate names its consumers.
That is the accepted cost of the one property nothing else can buy — a compile
error.
