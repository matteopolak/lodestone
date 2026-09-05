# Event routing — making a mis-routed `ClientEvent` a compile error

## What it is

`lodestone_model::event::route` is a single **exhaustive** table saying which of the client's
event routers claim each `ClientEvent` variant, so that adding a variant and forgetting to wire
it is a compile error rather than a silent nothing — the *island* defect class this repo's
architecture rules name as its most expensive recurring bug.

## How it works

`ClientEvent` is `#[non_exhaustive]`, so no crate outside `lodestone-model` can write an
exhaustive match over it — every downstream consumer necessarily ends in a wildcard arm, and
before this table existed a new variant compiled with **zero** routing arms anywhere and
reached nothing. `route(event: &ClientEvent) -> Route` is a single exhaustive match (no
wildcard arm) living beside the enum, inside the one crate that can write one:

```rust
pub struct Route {
    pub ingest: bool,            // lodestone_ecs::ingest  — per-entity ECS state
    pub session: bool,           // lodestone_ecs::session — local-player scalars
    pub shell: bool,             // lodestone_shell::net::forward — block/world state
    pub shell_conditional: bool, // that shell arm is guarded (see below)
    pub client: bool,            // consumed inside lodestone-client, not by a router
}
```

The flags are **not exclusive** — one event can be claimed by several routers at once. `Login`,
for example, writes an ECS entity component (`ingest`), a local-player scalar (`session`), a
shell `NetUpdate` (`shell`), and a client-only latch (`client`), all off one event.

The convention for which router owns a variant: **per-entity state → `ingest`**,
**local-player scalars → `session`**, **block/world state → `shell`** (which travels the
shell's own `NetUpdate` stream and needs no `handles_event` arm at all). Guessing the
`ingest`/`session` fork wrong has cost real work more than once — a debug feed keyed by
*subscription* is `session` even though it names an entity, because it outlives the entity's
own ECS row; a fold that *writes* a vehicle's own position is `ingest` even though the packet
carries no entity id. Ask what a fold **writes**, not what the packet is named or which nouns
appear in it.

`net::forward` stays a non-exhaustive catch-all (an exhaustive ~100-arm match would sit inside
a permanently contended choke-point file), but carries a `debug_assert!(!route(&event)
.must_forward(), …)`, where `must_forward()` is `shell && !shell_conditional` — so a
shell-routed variant with no forwarding arm fails loudly in every debug test rather than
quietly dropping, say, a chest-lid animation. Two arms in `forward` are deliberately guarded
(a literal block-break sub-event id, and a lightning-only entity-spawn filter) and so are
excluded via `shell_conditional`, or the assert would fire on correct traffic.

`Route::NOWHERE` (claimed by nobody) is a legal answer for an event with no consumer yet, but
it must be typed on purpose: `route_tests::route_has_no_catch_all_arm` refuses the tempting
`_ => Route::NOWHERE` rewrite that would restore exactly the silent-wildcard hole this table
exists to close. **Flipping a flag is not wiring** — the flag only decides who gets *asked*; a
router that is asked but has no system for the event still drops it silently. Write the system
and the flag together:
`ingest::tests::handles_event_covers_exactly_the_variants_with_a_system` is the runtime half of
the guarantee, feeding one instance of every claimed variant through the real schedule to prove
a system exists, not just that it was asked for.

### The island count

**22 of 134** variants are currently `Route::NOWHERE`. Most of those are simply decoded ahead
of a consumer, a normal state for a from-scratch client, not a defect in itself — but a handful
have been genuine islands where a fold already existed (or was cheap to add) and nothing fed
it, found by walking the list variant by variant and asking what a real consumer would need.
`lodestone_model::event::event_tests::the_island_count_in_the_docs_matches_this_source` derives
both numbers mechanically from `route`'s own source (the denominator from the variant count the
exhaustive match itself proves complete, the numerator from arms whose right-hand side is
exactly `Route::NOWHERE`, excluding the `..Route::NOWHERE` struct-update spread used inside arms
that set other flags) and fails if this line drifts from the real count — update the number and
the source together whenever a variant is added or wired.

A few variants are deliberately left `Route::NOWHERE` as **negative controls** for exactly this
table — e.g. one world-state scalar in the same subsystem family as several that were wired,
kept unwired on purpose so a fold that started matching too broadly would be caught by it
first; a gate asserts the premise (`route(&that_variant).is_island()`) before relying on it.
One other `Route::NOWHERE` entry is not a real island at all: a pushed resource-pack
acknowledgement is answered automatically, before the shell's event loop ever runs, from inside
`lodestone_client::driver::Driver::emit` itself — routing table silence is not proof of no
consumer when a reply is synthesized upstream of routing. The clientbound ping is explicitly
claimed by `Route::client` for the same reason. A transfer request is also explicitly claimed by
`Route::client`: the driver records its existing `SessionOutcome::Transferred` result before
surfacing the event, while the shell records the target for the resulting disconnect message.

## What this table does **not** do

- It does not measure whether a claimed router has a system — that is `handles_event`'s own
  coverage test.
- It does not cover the version adapters. Heavy per-block data (chunk payloads) reaches the
  world through `lodestone_world::WorldSink` directly and never becomes a routed event; a
  `ChunkLoaded`/`ChunkUnloaded` variant is marked `client` as a signal only.
- It does not cover the **serverbound** direction. `ClientAction` has the mirror problem (an
  action encoded by every adapter with zero producers) and has no equivalent table — see
  `docs/packet-wiring.md`.
- It is not `cargo xtask connectedness`, which measures clientbound decode → event wiring, a
  different axis entirely (decode existing at all, versus a decoded event being routed).

## How to change it

- **Adding a variant**: write the arm. `cargo check -p lodestone-model` refuses to compile
  until you do, and the compiler's own suggested `_ => todo!()`/`_ => Route::NOWHERE` fix
  restores exactly the wildcard hole this table exists to remove — do not take it.
- **Changing an existing route changes runtime behaviour.** Do it in its own reviewable commit,
  not as a drive-by while landing something else.
- **Update the island count in this doc in the same commit** as any change to `route()` — the
  gate above fails loudly naming both numbers when they drift, precisely so this line cannot
  go stale silently.

## Configuration

None. No features, no environment variables.

## Dependencies

- `crates/lodestone-model/src/event.rs` — `Route`, `route`, and the table; also
  `include_str!`s this very file to check the island count against its own source, so this
  file's path and the exact `**N of 134**` phrasing are load-bearing, not decorative.
- `crates/lodestone-ecs/src/ingest.rs`, `session.rs` — `handles_event`, each a one-line
  derivation of `route(e).ingest` / `route(e).session`.
- `crates/lodestone-shell/src/net.rs` — the `debug_assert!` in `forward`'s catch-all.
- `crates/lodestone-client/src/driver.rs` — client-internal responses and session outcomes,
  including transfer handling before the event is surfaced.
- `crates/lodestone-client/src/state.rs` — `SharedState::apply`, which unions the `ingest`/
  `session` predicates; unchanged by this table, it consults it transitively.

The layering is deliberately inverted: the leaf model crate names its own consumers by crate
path. That is the accepted cost of the one property nothing else can buy — a compile error.
