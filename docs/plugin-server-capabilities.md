# Server-side plugin capability parity

## What it is

A survey of what a server-side plugin can actually do today, set against the client's five-clause
intent doctrine (`docs/plugin-api.md`), and a design for what a *general* server-side capability
surface should look like once the server's own `bevy_ecs::World` (`crate::ecs` in
`lodestone-server`) grows past Phase 0. The client has one coherent doctrine covering every
player-verb seam; the server has five independently-shipped capability clusters, each answering
its own issue, each choosing its own subset of that doctrine — some choosing none of it. This
document names which is which, by symbol, and proposes the one addition (a general veto/adjudicate
layer riding the substrate's own `TickSet::Adjudicate`) that would make the pattern the crafting
hooks already discovered available to everything else, instead of being reinvented per feature.

## How it works

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
| Entity spawn/despawn | [`lodestone_server::IntegratedServer::spawn_mob`]/[`despawn_mob`], backed by [`crate::mobs::MobSim::remove_mob`] | — (direct calls: `spawn_mob(kind, pos)`, `despawn_mob(id)`) | yes — `MobHandle::with` is the one mutation path | no — a spawn/despawn either applies or the handle is `None` (no tick loop); there is no "another plugin said no" outcome at all | **missing entirely** — a second plugin cannot object to, delay, or observe-before-apply another plugin's spawn or despawn; it just happens | N/A — install/remove exist (spawn/despawn) but nothing between them is observable by a third party |
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

### The one gap that is a real hole, not a dropped clause

Entity spawn/despawn is the one capability in the table whose missing clauses are not defensible by
the same argument crafting hooks used. There genuinely *is* a second claimant an adjudication layer
would matter for: two independently-loaded plugins each deciding, on the same tick, whether to
spawn a mob at a location, or a world-population cap a different plugin wants to enforce against
every spawn regardless of source. Today `spawn_mob`/`MobSim::spawn_species` simply run — there is
no point at which a second plugin's opinion could be consulted, and no typed outcome a caller could
inspect to learn "a mob was spawned but something downstream immediately removed it," because
nothing downstream exists to do that.

This is not a criticism of `docs/plugin-entity-api.md`'s own scoping — that document says plainly
its own server-side half is built on `crate::mobs::MobHandle`, a pre-ECS primitive, specifically
*because* `crate::ecs` (Phase 0) moves no state yet and there is nowhere else to hang a veto. The
gap is real, and closing it is gated on the same thing every other future capability in this
document is gated on: `crate::ecs`'s `TickSet::Adjudicate` actually having systems in it.

### The substrate already has the right shape, and nothing uses it yet

`crates/lodestone-server/src/ecs/schedules.rs` declares [`TickSet`] with five members — `Drain`,
**`Adjudicate`**, `Apply`, `Simulate`, `Publish` — and `Adjudicate`'s own doc comment already states
the target design this document is proposing, almost verbatim: *"a protection plugin, an economy
plugin or a minigame manager gets a place in the schedule to say no before a proposal becomes world
state... server-side, the plugin outranks the client."* That is clauses (1)–(4) of the intent
doctrine, restated for the inverted-arbitration case, already written down — the set exists, it is
chained into `GameTick`, and Phase 2 of `docs/plans/server-ecs-migration.md` is where it gets
populated. **None of the five shipped capabilities above route through it.** Every one predates
Phase 0 or was built parallel to it, against whichever pre-ECS primitive already existed
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

### The recommendation

**Do not build a sixth, bespoke adjudication mechanism for entity spawn/despawn (or for whatever
capability lands next).** Build one general one, once, on `TickSet::Adjudicate`, shaped like this:

```rust
/// One proposed server-side action, in the observation vocabulary a plugin
/// reasons about — never a raw ClientAction or an internal id allocation
/// detail. `SpawnMob { kind, pos, source }` is the concrete first case;
/// the enum grows one variant per capability that adopts this layer.
#[derive(Event)] // bevy_ecs Message, matching `docs/plugin-api.md`'s `GameEvent` shape
pub enum ServerProposal { SpawnMob { kind: ResourceKey, pos: Vec3 }, /* ... */ }

/// A plugin's answer — the same three-way shape `StationVerdict` already
/// proved out, generalised past crafting.
pub enum ProposalVerdict { Allow, Deny, Replace(ServerProposal) }

/// Systems in `TickSet::Adjudicate` read `Messages<ServerProposal>` and
/// write into this per-proposal-id table; `TickSet::Apply`'s systems
/// consult it before doing anything the proposal described. First
/// non-`Allow` verdict wins — `CraftingStationHooks::evaluate`'s own rule,
/// unchanged.
```

This reuses, rather than reinvents, three things this repo has already built and tested: the
verdict shape (`StationVerdict`), the "first non-`Allow` wins, priority-ordered" rule
(`CraftingStationHooks::evaluate`, `EgressFilters`/`ActionVetoes`), and the schedule position
(`TickSet::Adjudicate`, already declared and already documented for exactly this purpose). What it
adds is the one thing none of the three prior art pieces needed on their own: a **shared** proposal
vocabulary two independently-authored plugins can both see, which is precisely what "two plugins
disagreeing about the same spawn" requires and what a bespoke per-capability hook (a second
`SpawnHooks` registry, mirroring `CraftingStationHooks` one-for-one) would not provide — a second
registry solves one capability's arbitration and leaves the next one to invent a seventh mechanism.

### Migration path, not a rewrite

Nothing above requires touching `spawn_mob`/`despawn_mob`'s existing signatures or breaking
`crates/lodestone-server/tests/native_plugin_spawns_and_despawns_a_mob.rs`, which is deliberate:

1. **Land `TickSet::Adjudicate`'s first real system** the day Phase 1 threads `&mut World` into
   `crate::tick::run_tick_loop` (`docs/plans/server-ecs-migration.md`'s own next step) — a plain
   `Messages<ServerProposal>` reader/writer pair, no capability wired to it yet. This is the
   substrate work this document's recommendation is gated on, and it is Phase 2's stated job, not
   new scope this document invents.
2. **`spawn_mob` gains a checked variant** (`spawn_mob_proposed`, or a feature flag on the existing
   one) that pushes a `ServerProposal::SpawnMob` and reads back a verdict from the `World` before
   calling `MobSim::spawn_species` — additive, so every existing caller (including the direct-call
   test above) keeps working unchanged until a host opts in.
3. **A second capability adopts the same enum** only once a second real need appears — do not
   pre-populate `ServerProposal` with every capability in this document's table "for completeness."
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

None. This document proposes no new crate, dependency, or runtime flag — `ServerProposal`/
`ProposalVerdict` would live in `crate::ecs` (or a small new sibling module) in `lodestone-server`,
the same crate every capability in the table above already lives in.

## Dependencies

`bevy_ecs`/`bevy_app`, already direct dependencies of `lodestone-server` (see the doc-drift note
above for why this is not "via `lodestone-ecs`"). No new crate is implied by the recommendation.

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
[`despawn_mob`]: ../crates/lodestone-server/src/integrated.rs
[`crate::mobs::MobSim::remove_mob`]: ../crates/lodestone-server/src/mobs/mod.rs
[`lodestone_server::plugin_crafting::CraftingStationHooks`]: ../crates/lodestone-server/src/plugin_crafting.rs
[`StationVerdict`]: ../crates/lodestone-server/src/plugin_crafting.rs
[`StationInputs`]: ../crates/lodestone-server/src/plugin_crafting.rs
[`TickSet`]: ../crates/lodestone-server/src/ecs/schedules.rs
[`docs/plugin-crafting-hooks.md`]: plugin-crafting-hooks.md
