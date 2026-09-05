# Server-side plugin capability parity

## What it is

A survey of what a server-side plugin can actually do today, set against the client's five-clause
intent doctrine (`docs/plugin-api.md`), and the first working slice of a *general* server-side
capability surface on the server's own `bevy_ecs::World` (`crate::ecs` in
`lodestone-server`). The client has one coherent doctrine covering every player-verb seam; the
server has independently-shipped capability clusters, each answering its own scope. This document
names which is which, by symbol, and records the bounded veto/adjudicate layer riding
`TickSet::Adjudicate` that the checked mob-spawn path now consumes.

## How it works

### Plugin-defined typed messages

Native server plugins share observation types with `#[derive(Message)]` and register each type
through `App::add_message::<T>()`. Producers use `MessageWriter<T>` or `World::write_message`;
independent consumers use `MessageReader<T>`. Registration is idempotent, so a consumer can register
the shared type before the producer plugin is installed. Order producer and consumer systems using
the server tick sets when delivery must happen in the same tick.

`ServerCorePlugin` runs Bevy's `message_update_system` before `TickSet::Drain`. The server drives
`GameTick` directly, so Bevy's frame-based maintenance schedule is never responsible for message
retention here. A message survives two maintenance boundaries: one written during a tick is readable
for the remainder of that tick and throughout the next. A reader that misses that window loses the
message; messages are not a durable queue. Each reader cursor sees a retained message at most once.
Scheduler callbacks run after maintenance, so their messages have the same lifetime as system writes.

Do not install another aging system for a type registered with `add_message`; that would shorten its
delivery window. Merely inserting `Messages<T>` as a resource does not register its maintenance.
`ServerProposal` is the built-in gameplay proposal vocabulary. The initial `SpawnMob` case carries
only entity key and position; plugins observe it with `MessageReader<ServerProposal>` and call
`ServerProposalDecisions::decide`. `Allow` leaves it unchanged, `Deny` reports a typed refusal,
and `Replace` supplies the action the single apply path will perform. Lower numeric priorities win;
equal priorities retain the first system to decide. The vocabulary has no durable history or
read-only monitor tier yet.

### Native tick scheduling

`lodestone_server::ecs::ServerTaskScheduler` is a resource installed by `ServerCorePlugin`.
Native plugins register callbacks during `ServerApp::bootstrap_with` or from a system's
`ResMut<ServerTaskScheduler>`. The primary world tick task runs `run_server_tasks` in
`TickSet::Drain` on both in-memory and persistent worlds, including the dedicated binary.
Callbacks receive `&mut World` and their `ServerTaskId`; no shared world lock is exposed.

`schedule_once(delay, callback)` fires after `max(delay, 1)` scheduler passes.
`schedule_repeating(delay, period, callback)` subsequently fires every `max(period, 1)` passes.
Boot does not advance this clock. A delay of 2 and period of 3 registered at startup therefore fires
on gameplay ticks 2, 5, 8. Equal deadlines preserve registration order. Deadlines use an ordered
queue, so a tick does not scan callbacks whose deadlines are still in the future.

`cancel(id)` returns whether the handle was live. It can cancel another callback due on the same
tick or the calling task itself, preventing its next repetition. The scheduler resource stays in
the world while callbacks run; work registered inside a callback starts no earlier than the next
pass. One-shot handles expire after execution. Tasks are transient and dropped with the world;
there is no persistence or runtime plugin unloading.

```rust,ignore
ServerApp::bootstrap_with(|app| {
    app.world_mut().resource_mut::<ServerTaskScheduler>()
        .schedule_repeating(2, 3, |world, id| {
            // Read or mutate the world's plugin resources here.
            let _ = world.resource_mut::<ServerTaskScheduler>().cancel(id);
        });
})
```

### Native off-tick hand-back

`ServerTaskScheduler::spawn_with_handback(work, hand_back)` extends that same scheduler surface
without exposing another `World` access path. `work` is parameterless and must return a `Send` value;
on native targets it runs on a named worker thread. Its `hand_back(value, &mut World)` closure is
queued for the primary tick task and runs from `run_server_tasks` in `TickSet::Drain`, after message
maintenance and before due delayed/repeating callbacks. The exact arrival tick is intentionally
nondeterministic, but the mutation site is not: only the tick owner receives `&mut World`.

The scheduler admits a default maximum of 64 jobs across both work still running and results waiting
to hand back. `spawn_with_handback` returns `ServerAsyncTaskError::Full` rather than growing a work
or completion queue, so a plugin must retry later or discard its own request. A completion keeps its
reservation until the tick owner has run or discarded its closure; consequently a worker never waits
behind an unbounded result backlog. `ServerTaskScheduler::with_async_hand_back_capacity` is useful
for an embedder that needs a smaller or larger explicit limit.

`cancel_async(id)` guarantees a result that has not reached its hand-back will not mutate the world;
it cannot forcibly interrupt native work already executing. `shutdown_async_tasks()` rejects new work,
marks outstanding jobs cancelled, and drops queued completions. Running workers finish and release
their reservations without a world callback. Scheduler drop invokes the same shutdown, so a stopped
world has no return route from a worker. A panicking work closure is discarded. On `wasm32`, where
there are no worker threads, work executes inline but its result still uses the next scheduler pass
and the same bounded hand-back queue.

```rust,ignore
let result = app.world_mut().resource_mut::<ServerTaskScheduler>()
    .spawn_with_handback(
        || load_plugin_owned_data(),
        |data, world| world.insert_resource(data),
    );
// `Full` is backpressure, not a request to block the tick thread.
```

### The client's doctrine, restated as five questions

`docs/plugin-api.md`'s intent doctrine is five clauses. Read as questions a capability answers:

1. Does a plugin see *observation* vocabulary (`BreakIntent { pos, face }`) or a wire/internal
   detail (a sequence number, a raw `ClientAction`)?
2. Is there exactly one system/function that owns applying the effect?
3. Is a refusal always observable (a typed outcome), never a silent no-op?
4. Is there a second, human source of the same action to arbitrate against, and if so, who wins?
5. Does the capability have a lifecycle (install/remove, continuous vs one-shot), and does the API
   shape match it?

Client-side, the answer to (4) is always "the human, unconditionally, no handshake" — clause 4's
own text. Server-side there is no local human, so (4) is either "N/A, nothing to outrank" or, per
`docs/dedicated-server.md`'s own framing of the still-unbuilt adjudication window, inverted: *"the
plugin outranks the client's proposal, not the reverse."* That inversion is real and it is the
single biggest reason a server capability cannot just reuse the client's shape unmodified — it is
not a smaller version of the same doctrine, it is the same doctrine with one clause's answer
flipped and, in every capability shipped so far, with clauses 2/3 kept and clauses 4/5 dropped
rather than faked (see the crafting-station hook decision below, which the second half of this
table generalises).

### The five shipped capabilities, by symbol, scored against the five clauses

| capability | symbol path | (1) observation vocab | (2) single writer | (3) refusal observable | (4) human/plugin arbitration | (5) lifecycle-shaped |
|---|---|---|---|---|---|---|
| Worldgen: custom generator | [`lodestone_worldgen::generator::ChunkGenerator`] | — (a `dyn` trait a plugin *implements*, not an event it *observes*) | yes — one trait object per dimension key | N/A — there is no refusal; the plugin's output *is* the terrain | N/A — nothing else supplies terrain for the same column | N/A — a generator has no lifecycle beyond existing |
| Worldgen: custom dimension | [`lodestone_server::plugin_dimension::DimensionRegistry`] | — (a registration call, `register(dimension)`) | yes — `Option<Arc<PluginDimension>>` keyed by string, one owner per key | partial — `register` returns `None` on a duplicate key, so *that* refusal is observable; there is no other refusal shape | N/A | N/A — register once, `get`/`chunk_source` forever after |
| Worldgen: live structure placement | [`lodestone_server::structure_placement::place_structure_live`] | — (a direct function call with a template and origin) | yes — one function, called synchronously | no — returns a plain `usize` (cells written), no verdict a second party could have vetoed | N/A — nothing else contests one placement call | N/A — one-shot, matches its own shape |
| Entity spawn/despawn | [`lodestone_server::IntegratedServer::spawn_mob_proposed`]/[`despawn_mob_proposed`], backed by [`crate::mobs::MobSim::remove_mob`] | yes — `ServerProposalAction::{SpawnMob, NaturalSpawnMob, DespawnMob}` is observable; legacy `spawn_mob` and `despawn_mob` remain direct | yes — the checked path resolves exactly one action before `MobHandle::with` mutates | yes — checked spawn/despawn return typed `Denied`, `TimedOut`, `Unavailable`, or a mismatched replacement; a permitted missing despawn reports `Ok(false)` | yes — native plugins prioritize allow/deny/replace for checked spawn, natural candidates, and checked despawn; lower numeric priority wins and ties keep schedule order | N/A — install/remove remain one-shot actions, not long-lived subscriptions |
| Crafting-station hooks | [`lodestone_server::plugin_crafting::CraftingStationHooks`], [`StationVerdict`] | **yes** — [`StationInputs`] is observation-only: the station, its input cells, vanilla's own computed result; never a menu-slot index, a raw click, or a mutable inventory borrow | **yes** — `workstation_result` is the one choke point every one of the five production entry points already passed through before this work | **yes** — `StationVerdict::{Allow, Deny, Replace(ItemStack)}`, always returned, never inferred from silence | **dropped, by name** — "there is no second, *human* source of a workstation result to arbitrate against ('human outranks a plugin' has nothing to outrank)" | **dropped, by name** — "a station evaluation has no lifecycle beyond answering the one question it was asked" |

Reading the table by column rather than by row is the actual finding. Column (1): only crafting
hooks give a plugin a genuine observation struct; everything else is a direct call in either
direction (a plugin calling the engine, or nothing calling the plugin at all). Column (3): only
crafting hooks and (partially) dimension registration have a typed refusal; entity spawn/despawn
has *no* refusal shape, because there is nothing yet that could refuse it. Columns (4)/(5): every
capability either has nothing to arbitrate (worldgen, structures — there is no second claimant) or
drops the clause explicitly and by name (crafting hooks) — **except entity spawn/despawn, which
has something to arbitrate (two plugins, or a plugin and the world's own mob cap, disagreeing about
whether a spawn should happen) and currently provides no mechanism for it at all.** That is this
survey's one concrete, specific gap, not an abstract "parity is incomplete" — see "The one gap that
is a real hole, not a dropped clause" below.

### Crafting hooks got the shape right, on the first attempt, for a documented reason

[`docs/plugin-crafting-hooks.md`]'s own text is the clearest statement in the repo of why *this*
capability, alone among the five, ended up matching three of the client's five clauses: vanilla's
own `PrepareAnvilEvent`/`PrepareSmithingEvent`/`PrepareItemCraftEvent` already have exactly this
shape (an observation, a verdict, first-non-`Allow`-wins), so porting the vanilla event model *was*
porting three-fifths of the intent doctrine, without anyone setting out to reuse it. The two
dropped clauses were dropped by argument, not by omission — "there is no second, human source" and
"no lifecycle beyond answering one question" are both true statements about what a crafting-station
read is, not gaps the author failed to notice. That is the reusable template: **when a capability
resembles a vanilla `PrepareXEvent`, port the event's own Allow/Deny/Replace shape; when it doesn't
resemble one, decide clauses 4 and 5 by argument, in the doc, the way the crafting-station hooks
did — never default to
either "shipping only Allow" (silently dropping clause 3) or "faking a human to outrank" (clause 4
answered with a fabrication instead of an argument).**

### The first real hole: checked spawn

Checked spawn closes the first real adjudication hole. A caller awaits
`IntegratedServer::spawn_mob_proposed`; its bounded ingress reaches the primary tick task's
`TickSet::Drain`, plugins see the proposal during `TickSet::Adjudicate`, and `TickSet::Apply`
returns the final action before `MobHandle::with` runs. Plugins run only on the tick task, and the
caller awaits without either a world lock or a mob-handle lock. The bounded queue and one-second
response deadline make a stopped or overloaded tick task observable as `Unavailable` or
`TimedOut`, rather than silently applying a late mutation.

The legacy direct methods intentionally remain outside this layer for compatibility. Checked despawn
and population-driven spawn now share it: the tick loop plans the natural candidates after a short
mob census lock, stages `NaturalSpawnMob` actions, runs `GameTick`, then takes the resolutions and
materializes only accepted candidates under a fresh lock. No adjudicator runs while `MobHandle` is
held. Automatic distance-based despawn and the remaining non-spawn capabilities do not yet submit
`ServerProposal`s.

### The substrate now has one production consumer

`crates/lodestone-server/src/ecs/schedules.rs` declares [`TickSet`] with five members — `Drain`,
**`Adjudicate`**, `Apply`, `Simulate`, `Publish` — and `Adjudicate`'s own doc comment already states
the target design this document is proposing, almost verbatim: *"a protection plugin, an economy
plugin or a minigame manager gets a place in the schedule to say no before a proposal becomes world
state... server-side, the plugin outranks the client."* That is clauses (1)–(4) of the intent
doctrine, restated for the inverted-arbitration case, already written down — the set exists, it is
chained into `GameTick`, and Phase 2 of `docs/plans/server-ecs-migration.md` is where it grows
beyond this first case. `IntegratedServer::spawn_mob_proposed` is the current production route
through it. The other capabilities predate Phase 0 or were built parallel to it, against whichever
pre-ECS primitive already existed
(`MobHandle`, `WorldStateHandle`, a plain registry) — which is the correct call for each of them
individually (there was nothing else to build against), and is exactly why this survey exists: the
five capabilities are real, individually well-reasoned, and collectively inconsistent, because each
solved its own problem before there was a shared place to solve the general one.

**A doc drift worth naming while it's in scope.** `docs/dedicated-server.md`'s "Server-side ECS"
section currently states `lodestone-server` links `bevy_ecs` "via `lodestone-ecs`". That is stale:
`crates/lodestone-server/Cargo.toml` depends on `bevy_app`/`bevy_ecs` **directly**, with its own
comment stating the opposite explicitly — *"Deliberately NOT `lodestone-ecs`... linking that crate
would drag the entire client vocabulary... into this graph"* — and `crate::ecs`'s own module doc
repeats the same point (`schedules.rs`: *"Do not add `lodestone-ecs` to this crate without
re-running `scripts/wasm-size.sh`"*). Flagged rather than fixed here — `docs/dedicated-server.md` is
outside this session's file ownership.

## How to change it

Message maintenance belongs in `ServerCorePlugin`, before the first gameplay set. The `ecs::messages`
tests assert exact retention boundaries, and `independent_plugins_exchange_bounded_messages_on_the_primary_tick_task`
tests delivery between two separately registered plugins against the production server constructor.
Add shared observation types in the plugin's public API and keep their contents version-free.

The synchronous scheduler lives in `ecs/scheduler.rs`; registration and its schedule anchor live in
`ServerCorePlugin`. Systems sharing resources with scheduled callbacks must order themselves before
or after `run_server_tasks` if they also occupy `TickSet::Drain`. Keep the resource installed during
callback execution so nested scheduling and cancellation remain valid. The dedicated binary's
`dedicated_scheduler_runs_delayed_work_on_the_persistent_primary_world` test asserts exact observed
counts at every tick, including cancellation before a would-be third repetition.

### Extending the implemented mechanism

**Do not build a bespoke adjudication mechanism for despawn or the next capability.** Extend the
existing `ServerProposal` mechanism on `TickSet::Adjudicate`:

```rust
/// One proposed server-side action, in the observation vocabulary a plugin
/// reasons about — never a raw ClientAction or an internal id allocation
/// detail. The action enum grows one variant per capability that adopts this
/// layer.
#[derive(Message)]
pub struct ServerProposal { id: u64, action: ServerProposalAction }

/// A plugin's answer — the same three-way shape `StationVerdict` already
/// proved out, generalised past crafting.
pub enum ProposalVerdict { Allow, Deny, Replace(ServerProposalAction) }

/// Systems in `TickSet::Adjudicate` read `Messages<ServerProposal>` and
/// write into this per-proposal-id table; `TickSet::Apply`'s systems
/// consult it before doing anything the proposal described. The lowest numeric
/// priority wins; equal priorities retain the first decision.
```

This reuses the verdict shape, the priority-ordered rule, and the schedule position
(`TickSet::Adjudicate`, already declared and already documented for exactly this purpose). What it
adds is the one thing no direct mutation path needed on its own: a **shared** proposal
vocabulary two independently-authored plugins can both see, which is precisely what "two plugins
disagreeing about the same spawn" requires and what a bespoke per-capability hook (a second
registry) would not provide — a second registry solves one capability's arbitration and leaves the
next one to invent another mechanism.

### Next adoption, not a rewrite

Nothing above requires touching `spawn_mob`/`despawn_mob`'s existing signatures or breaking
`crates/lodestone-server/tests/native_plugin_spawns_and_despawns_a_mob.rs`, which is deliberate:

1. **Keep `spawn_mob` direct** while callers migrate deliberately to async
   `spawn_mob_proposed`; the direct-call test remains the compatibility control.
2. **Add a second action only with a production owner.** Checked despawn and natural spawn now use
   the same ingress/message/decision/apply pass; future automatic despawn must follow the same
   no-callback-under-`MobHandle` split rather than adding a direct hook.
3. **Do not pre-populate `ServerProposal`** with every capability in this document's table. Add an
   action only once a second real need appears; a speculative variant has no production consumer.
   Crafting hooks stay on `StationVerdict` regardless: `docs/plugin-crafting-hooks.md`'s own
   argument for why clauses 4/5 do not apply there is unaffected by this document, and there is no
   second human/plugin claimant for a workstation read the way there is for a spawn — migrating it
   onto `ServerProposal` would be change for its own sake, not a real parity gain.

### What this document is *not* proposing

- **Not** retrofitting worldgen/dimension/structure placement onto `ServerProposal`. None of the
  three has a second claimant to arbitrate against (a `ChunkGenerator` is the sole source of terrain
  for its own column; a `place_structure_live` call is a direct, synchronous edit nothing else is
  simultaneously proposing). Clauses 4/5 are N/A there for the same reason they are N/A on the
  client for, say, a resource-pack override — there being only one possible actor is a valid answer,
  not a gap.
- **Not** a WASM-tier equivalent yet. `docs/plugin-api.md`'s own WASM host has no server-side
  counterpart at all today (`crates/lodestone-wasm-host` is client-only), and nothing in this
  document's table depends on one existing — every capability surveyed is native-tier, Rust-crate
  plugins, which is the only server-side tier that exists to have a parity conversation about.
- **Not** a claim that Phase 2 should start now, as part of this issue. This document is the
  read-only architecture review this design is answering; `docs/plans/server-ecs-migration.md` is where the
  phased implementation work is tracked and estimated.

## Configuration

Scheduling uses integer gameplay-tick delays and periods; no environment variable or runtime flag
is required. Delays, repeats, and handles reject `u64` overflow rather than wrapping.
The adjudication layer adds no crate, dependency, or runtime flag. It lives in
`lodestone_server::ecs::proposals`; its queue holds 64 in-flight requests and its async caller
deadline is one second.

## Dependencies

`bevy_ecs`/`bevy_app` and the existing Tokio runtime, already direct dependencies of
`lodestone-server` (see the doc-drift note above for why this is not "via `lodestone-ecs`").

## See also

- [`plugin-api.md`](plugin-api.md) — the client-side intent doctrine this document scores every
  server-side capability against.
- [`plugin-worldgen-api.md`](plugin-worldgen-api.md), [`plugin-entity-api.md`](plugin-entity-api.md),
  [`plugin-crafting-hooks.md`](plugin-crafting-hooks.md) — the three capability clusters this
  document surveys; read those for the full implementation detail behind each table row.
- [`dedicated-server.md`](dedicated-server.md) — the server's tick loop and its "Server-side ECS"
  section, whose adjudication-window framing this document builds on directly (and whose
  `lodestone-ecs` claim is stale — see "The substrate already has the right shape" above).
- [`packet-wiring.md`](packet-wiring.md) — `ActionVetoes`/`EgressFilters`, the client-side
  equivalent of "first non-`Allow` verdict wins," predating and matching `StationVerdict`'s shape.
- [`docs/plans/server-ecs-migration.md`](plans/server-ecs-migration.md) — the phased plan
  `TickSet::Adjudicate`'s population belongs to; this document's recommendation is scoped as future
  work on top of that plan's Phase 2, not a competing plan.

[`lodestone_worldgen::generator::ChunkGenerator`]: ../crates/lodestone-worldgen/src/generator.rs
[`lodestone_server::plugin_dimension::DimensionRegistry`]: ../crates/lodestone-server/src/plugin_dimension.rs
[`lodestone_server::structure_placement::place_structure_live`]: ../crates/lodestone-server/src/structure_placement.rs
[`lodestone_server::IntegratedServer::spawn_mob`]: ../crates/lodestone-server/src/integrated.rs
[`lodestone_server::IntegratedServer::spawn_mob_proposed`]: ../crates/lodestone-server/src/integrated.rs
[`lodestone_server::IntegratedServer::despawn_mob_proposed`]: ../crates/lodestone-server/src/integrated.rs
[`despawn_mob`]: ../crates/lodestone-server/src/integrated.rs
[`crate::mobs::MobSim::remove_mob`]: ../crates/lodestone-server/src/mobs/mod.rs
[`lodestone_server::plugin_crafting::CraftingStationHooks`]: ../crates/lodestone-server/src/plugin_crafting.rs
[`StationVerdict`]: ../crates/lodestone-server/src/plugin_crafting.rs
[`StationInputs`]: ../crates/lodestone-server/src/plugin_crafting.rs
[`TickSet`]: ../crates/lodestone-server/src/ecs/schedules.rs
[`docs/plugin-crafting-hooks.md`]: plugin-crafting-hooks.md
