# The server's own `bevy_ecs::World`

## What it is

The architectural decision, and the design it produced, to link `lodestone-ecs` into
`lodestone-server` so that server-side plugins get the same five-clause intent doctrine
[`docs/plugin-api.md`](./plugin-api.md) already gives client-side plugins — one system per
machine, always-observable refusal, lifecycle-encodes-verb-shape — instead of a second,
hand-rolled plugin idiom. This reverses [`docs/server-tick-loop.md`](./server-tick-loop.md)'s
recommendation not to link `lodestone-ecs` into the server (see that doc's own "Recommendation
reversed" section for why each of its three original reasons no longer blocks). This document is
the decision record
and the design five queued implementation phases build against — **nothing in
`crates/lodestone-server/` implements this yet**. Where this document describes behaviour, it is
describing the design, not shipped code; where it cites `crates/lodestone-ecs/` today, that code
already exists and this document is reporting it directly.

The motivating constraint, restated because it is the reason this exists at all: the owner's
standing rule is that the plugin API and the internal API are the same thing, with core
functionality implemented through the plugin surface — Bukkit/Spigot's own shape. Real Bukkit/Paper
plugins are overwhelmingly *server* plugins (protection, economy, permissions, minigames), and
until this decision the entire intent doctrine lived in the client's `World` only. The design
decision named three costed options (adopt an ECS server-side, grow a bespoke hook surface, or
defer to a separate JVM tier); the owner chose the first.

## How it works

### Two `World`s, never one, and why

The client keeps its `bevy_ecs::World`; the server gets its **own**, independent `World`, owned
outright by the tick task. They are never merged, for three independent reasons, each on its own
sufficient:

**(a) Contradictory clock policies.** The client's `GameTick` schedule is driven by `FrameClock`,
whose accumulator *discards* excess catch-up past `MAX_CATCH_UP_SECS` (10 ticks,
`lodestone_ecs::MAX_CATCH_UP_TICKS`, `crates/lodestone-ecs/src/resources.rs`; the clamp
itself in `FrameClock::begin_frame`, `self.accumulator += dt.clamp(0.0, MAX_CATCH_UP_SECS)`) — right for a
predicting client, where replaying a minute of stalled ticks in a burst is worse than dropping it.
The server has the opposite obligation: it must keep advancing when the render loop stalls or is
absent entirely (open-to-LAN has no render loop at all), replay small backlogs the way
`docs/server-tick-loop.md`'s overrun handling already does, and only forgive a backlog past
vanilla's own 2-second overload threshold, with vanilla's warning. A `World` carries exactly one
schedule and one accumulator by construction — `docs/world-unification.md`'s whole §4.1(c) is the
record of what happens when two clocks quietly diverge on one schedule. Two contradictory
catch-up policies cannot both own one accumulator, so they need two `World`s.

**(b) The singleplayer/multiplayer parity failure mode.** Singleplayer here is *already*
structurally multiplayer: `IntegratedServer::open_in_memory` (and its
`_with_entities`/`_with_mobs` siblings, in `crates/lodestone-server/src/integrated.rs`)
hands back a real `tokio::io::DuplexStream` and serves genuine protocol bytes over it — there is no
"local shortcut" path that skips the wire. A shared `World` would punch straight through that: a
client-plugin system could read server-authoritative state that simply does not exist when the
same client later connects to someone else's real server. A plugin that works in singleplayer and
silently breaks (or, worse, cheats undetectably and then gets banned) on a real server is the worst
outcome available in a plugin framework whose entire premise is "everything native can do" — it
would make singleplayer a bad emulator of the actual runtime environment.

**(c) Different representations, different keys.** The client's world is version-decoded client
state (chunk sections, entities keyed by client-assigned `bevy_ecs::Entity`, built from wire bytes
one specific protocol family produced). The server's is version-free simulation state
(`lodestone_world::World` via `ChunkWorld`, `lodestone_game`'s canonical aggregates). Merging them
would drag version-decoded state into the version-free server — exactly the coupling
`docs/plugin-api.md`'s "what stays privileged" section keeps out of `lodestone-ecs` on purpose.

### The server owns its `World` with no lock at all

This is the most useful thing a future reader can learn from this document, so it is stated
plainly: **the server's `World` is held directly by the tick task, not behind
`Arc<parking_lot::RwLock<World>>`.** Not `EcsHandle`. The client needs that handle because multiple
tasks — the driver, the net thread, async bot callers — all need concurrent access to one `World`
on one process, and `docs/world-unification.md`'s entire "Lock discipline" section (three rules, a
reentrancy ledger, a `LockHolds` meter, a hard client freeze once already shipped from exactly this)
exists to make that safe. None of that need exists server-side by construction, because nothing
else needs to *read* the server's `World` concurrently with the tick task mutating it — every
connection task's job, in this design, is to enqueue proposals and read published snapshots, never
to reach into the `World` directly. So the entire lock-discipline hazard class —
hold-duration ledgers, reentrancy tripwires, the ABBA ordering rule between the `World` and the
chunk store, cross-thread hang potential — **simply does not exist on the server side.** This is
better than the client's arrangement, not merely different from it: the client pays that
complexity because it has no choice, and the server's design gets to not pay it at all.

### The plugin model is Fabric's client/server split, not Bukkit's server-only

A Bukkit plugin is server-only by construction — there is no client half to speak of. A Lodestone
plugin can have both, and the two halves are genuinely separate: a server-side plugin's `World`
never contains a `LocalPlayer`, `FrameClock`, or anything client-only, and a client-side plugin's
`World` never contains the server's authoritative simulation state. The only channel between them
is the wire vocabulary already in `lodestone-model` — `ClientEvent`
(`crates/lodestone-model/src/event.rs`) and `ClientAction`
(`crates/lodestone-model/src/action.rs`) — the same vocabulary a real network connection would
use, because in two of the three deployments below, a real network connection *is* what is
carrying it.

| deployment | what a client-side plugin sees | what a server-side plugin sees |
|---|---|---|
| singleplayer (integrated server) | the client's `World`, fed by real wire bytes over an in-process `DuplexStream` | the server's `World`, fed by the same wire bytes from the other end of that stream |
| open-to-LAN | the client's `World`, same as singleplayer | the server's `World`, now also fed by real TCP connections from other players |
| joining a remote server | the client's `World` only — there is no server half in-process at all | not applicable — no server plugin runs unless *you* are hosting |

The confusion this table exists to prevent: nothing about "the client and server are in the same
process during singleplayer" makes their `World`s the same `World`, and nothing about "a plugin can
do everything native can" means a client-side plugin can *name* the server's `World`. It is never
wrapped in `EcsHandle`, never exposed through any type client-side code depends on, and never
leaves `lodestone-server` — a client-side plugin author has no import path that reaches it, by
construction, not by convention.

### The never-straddle invariant

**Every piece of state is classified simulation or replication, with exactly one owner.**

- **Simulation** — anything two connections must agree on, or that must keep advancing with no
  connection attached at all — lives in the server `World`, mutated only inside `GameTick` on the
  tick thread.
- **Replication** — per-connection cursors, last-sent caches, socket health — lives in the
  connection task, and must always be reconstructible from (authoritative state × that connection's
  cursor). Nothing here needs to survive the connection, and nothing here is a source of truth for
  anything else.

This is the rule; the finding that produced it is worth recording on its own, because it is the
valuable part. `docs/server-tick-loop.md`'s six-timer table lists four per-connection timers left
out of the unified `run_tick_loop`, and it would be easy to read all four as one category ("things
that stayed per-connection because they're per-connection"). They are not one category. All four
are `const`s in `crates/lodestone-server/src/server.rs`:

| timer | classification | why |
|---|---|---|
| `KEEP_ALIVE_INTERVAL` | replication | a health check for one socket; nothing to replay for a new connection |
| `TIME_SYNC_INTERVAL` | replication | re-derivable from world time × this connection's last-sent value |
| `CONTAINER_SYNC_INTERVAL` | replication | diffs against what *this* connection was last sent; a second connection has its own cursor |
| `VITALS_TICK_INTERVAL` | **simulation** | a player's health/food/saturation is authoritative state — it lives per-connection today only because a player is not yet server-`World` state, and there is at most one player per connection |

Three of the four are correctly per-connection replication. `VITALS_TICK_INTERVAL` is the exception:
a player's vitals are exactly the kind of fact "two connections must agree on" describes (a second
player attacking this one needs the *server's* view of their health, not a copy this connection
happens to hold), and the only reason it survives as a per-connection timer today is that a
connected player has no server-`World` entity to be a component of yet. The original author drew a
line — "world-owned state stays unified, per-connection state stays where it is" — that happened to
be right for three of four cases without ever writing down the actual rule. "World versus
connection" was the approximation that got it right by accident; "simulation versus replication" is
the rule that explains *why*, and this migration is what gives vitals a `World` entity to live on
instead of a per-connection scalar.

### The straddle already exists, with no ECS involved

**This migration fixes an existing violation of the never-straddle invariant; it does not risk
creating a new one.** Two inline mutations already cross from the connection task straight into
shared simulation state, today, with no scheduling in between:

- `apply_block_action` (`crates/lodestone-server/src/server.rs`) calls `source.set_block(...)`
  directly from inside `dispatch_play_packet` (the per-connection packet
  dispatcher) — on a confirmed break (`StopDestroy`), and on a placement in
  `apply_use_item_on`. `source` is the shared `ChunkSource` every
  connection reads and writes.
- `apply_attack` (`server.rs`) mutates the shared mob simulation directly —
  `mobs.with(|sim| sim.attack(...))` — called from `dispatch_play_packet`.
  `mobs` is the same `MobHandle` `crate::tick::run_tick_loop` ticks
  and publishes from.

Both are simulation state (a block everyone sees, a mob everyone can hit) mutated inline, on
whichever connection task happens to be handling that packet, with no scheduling boundary and no
way to interpose anything between "packet arrived" and "world changed." That is the straddle the
invariant above forbids — it simply predates having a name for it. Moving packet-apply into
`GameTick` systems does not introduce this risk; it removes an instance of it that already shipped.

### The adjudication window is the point — lead with this

**This is the single strongest architectural argument for the whole migration.** Inline-apply in a
connection task has nowhere to put a veto: by the time `apply_block_action` runs, the block is
already set. A scheduled apply changes that completely. Once packet-apply happens as a system
inside a schedule, an `Adjudicate` set sits naturally between "drain inbound proposals" and "apply
them to the `World`" — and once that ordering exists, cancellation, `Lowest..Monitor`-style priority
ordering, and a `MONITOR`-equivalent observe-only tier all fall out of it for free, the same way
Bukkit's own event-priority model works. A protection plugin vetoing a break in a claimed region, an
economy plugin taxing a transaction, a minigame manager freezing block edits between rounds — none
of these are expressible against an inline `source.set_block` call. All of them are a system
ordered before `Adjudicate`'s consumer, given the chance to say no first.

### How the intent doctrine changes server-side

`docs/plugin-api.md`'s five clauses were written client-side. Checked against a server plugin's
actual role, one at a time:

1. **Wishes in observation vocabulary, never wire vocabulary — survives unchanged.** A server
   plugin still expresses "place this block" or "damage this entity" as a fact about the world, not
   a packet shape; the server already owns packet encoding entirely, so there is no wire-vocabulary
   temptation to guard against that the client doesn't already have.
2. **Exactly one system owns each machine — survives unchanged.** The single-writer discipline this
   clause states is exactly what an adjudicated `GameTick` system gives every piece of simulation
   state once packet-apply moves into a schedule.
3. **Refusal is always observable — survives, but its consumer changes.** Client-side, refusal is
   an always-present `Outcome` component a plugin polls. Server-side, the plugin *is* often the
   thing doing the refusing (clause 4, below) — the party that needs to *observe* a refusal is the
   remote client, and the mechanism that already exists for telling a client "no, actually" is
   vanilla's own corrective packet: a block-update reverting a predicted placement, a health packet
   reverting a predicted hit. Refusal stays observable; what it is observed *by* flips from a plugin
   polling a component to a wire packet a real client receives.
4. **Human input outranks installed intent — inverts, and this is the one clause that actually
   changes.** Client-side, the human at the keyboard outranks a plugin's intent, because the human
   is the ground truth the client exists to serve. Server-side there is no local human: the remote
   client's input arrives as a *proposal*, not ground truth, and the plugin — protection, a
   minigame, an anticheat — is precisely the thing entitled to overrule it. This is the adjudication
   window from the previous section, restated as doctrine: server-side, the plugin outranks the
   client, not the reverse.
5. **Lifecycle encodes verb shape — mostly does not transfer.** `BreakIntent`/`PlaceIntent`'s
   insert-then-remove lifecycle exists to let a *wisher* express a continuous-or-one-shot desire
   that some other system may or may not satisfy. A server plugin is not a wisher — it is
   authoritative, so it does not need a lifecycle-encoded wish at all. What it needs, and what
   clause 4's inversion actually delivers, is the adjudication window itself: a place in the
   schedule to say yes or no before a proposal becomes world state. Intent *components* mostly do
   not have a server-side equivalent to convert to; the adjudication set is the thing that matters.

## How to change it, and the gotchas

- **Do not install `CorePlugin` on the server's `App`.** Its `Plugin::build` (`crates/lodestone-ecs/src/plugin.rs`)
  inserts `FrameClock` (`init_resource::<crate::FrameClock>()`) and configures `Update`'s
  `FrameSet::{Input, Interpolate, Camera, Terrain}` chain — both are lies in a server `World`: there
  is no frame, no camera, and no render-driven `Update` schedule to chain against. The server needs
  its own core plugin that installs `WorldTime`, the `NetIngest`/`GameTick`/`Extract` schedules and
  sets it actually uses, and nothing frame-shaped.
- **Namespace pollution is real but non-blocking.** A server-plugin author writing `use
  lodestone_ecs::*` sees `LocalPlayer`, `SessionMenus`, `FrameClock` — none of which exist in their
  `World`, and none of which will panic if named (a query against a component nobody ever inserts
  just returns nothing) but all of which are noise in autocomplete and in "what can I even do here."
  The end-state fix is splitting `lodestone-ecs` into a substrate crate (schedules, generic ECS
  plumbing) and a client-vocabulary crate (`LocalPlayer` and friends) — real churn to the plugin
  ABI's import paths, so it is correctly a **follow-up**, not a prerequisite blocking this migration.
- **wasm binary size is the second non-blocking cost.** The server already compiles for
  `wasm32-unknown-unknown` for browser singleplayer (`docs/bevy-migration.md` §8,
  `crates/lodestone-server/Cargo.toml`'s `[target.'cfg(target_arch = "wasm32")'.dependencies]`
  block). Adding `bevy_app`/`bevy_ecs` grows that binary; both are already `default-features = false,
  features = ["std"]` with no `bevy_reflect`, which is the mitigation already in place for the
  client side of the same tradeoff. Watch it, do not block on it.
- **Classify new per-connection state before adding it.** Per the never-straddle invariant above:
  ask "does any other connection need to agree with this?" before putting a new timer or cache in
  the connection task. If yes, it is simulation and belongs in the `World`, mutated only from a
  `GameTick` system; if no, it is replication and the connection task is exactly where it belongs.
- **A `Resource` a plugin orders against must be genuinely `'static`-owned**, the same constraint
  `docs/plugin-api.md`'s "two Stage-1 constraints" section documents client-side — `bevy_ecs`'s
  `Resource: Send + Sync + 'static` bound does not relax for the server.

## Configuration

None yet — there is no server-side plugin-loading mechanism, feature flag, or manifest to
configure, for the same reason `docs/plugin-api.md`'s own "Configuration" section gives for the
client: a plugin today is a `Cargo.toml` dependency added with `App::add_plugins`, and this document
records the decision to build that surface, not the surface itself.

## Dependencies

- `bevy_app`, `bevy_ecs` — pinned via the same `[workspace.dependencies]` entries `lodestone-ecs`
  already builds against (root `Cargo.toml`, `version = "0.19", default-features = false,
  features = ["std"]`, never `multi_threaded` — see the "empirically void" leg in
  `docs/server-tick-loop.md`'s reversed recommendation for why that specific feature omission is
  what makes this migration free of a second threading model).
- `lodestone-game`, `lodestone-world` — the two new Lodestone crate edges this migration adds to
  `lodestone-server`'s graph, both already depended on by `lodestone-ecs` itself
  (`crates/lodestone-ecs/Cargo.toml`) and both version-free on inspection of their own manifests
  (`lodestone-game`'s dependencies are `lodestone-model` plus optional version-free
  `lodestone-core`/`lodestone-net`/`lodestone-registry`; `lodestone-world`'s are `lodestone-core`,
  `lodestone-testsupport`, `lodestone-worldgen` — no `lodestone-v*` crate in either).
- **The version-free property is not threatened by any of this.** `lodestone-ecs` itself depends on
  no version crate — confirmed directly against `crates/lodestone-ecs/Cargo.toml` (`bevy_app`,
  `bevy_ecs`, `parking_lot`, `lodestone-model`, `lodestone-physics`, `lodestone-entity`,
  `lodestone-game`, `lodestone-world`, `uuid`; no `lodestone-v47`/`v340`/`v735`/`v770`) — and
  `cargo xtask check-isolation` enforces protocol-version-crate dependency isolation workspace-wide
  today. Run live for this document (`cargo run -p xtask -- check-isolation`): it reports zero
  violations touching `lodestone-ecs` or `lodestone-server`. The only violations it currently reports
  are `lodestone-fuzz` depending directly on all four version crates — a real, pre-existing isolation
  gap, unrelated to this decision and not something this migration introduces or fixes.

## See also

- [`docs/server-tick-loop.md`](./server-tick-loop.md) — the tick loop this migration threads a
  schedule through, and the doc whose linking recommendation this reverses.
- [`docs/world-unification.md`](./world-unification.md) — the client's one-`World` migration this
  one mirrors on the server side, and the lock-discipline section that explains, by contrast, why
  the server needs none of that machinery.
- [`docs/plugin-api.md`](./plugin-api.md) — the five-clause client-side intent doctrine this
  document's "How the intent doctrine changes server-side" section maps onto the server.
- **"One `World`, one `GameTick`, one accumulator" as a permanent decision for the plugin ABI.**
  Two `World`s here does not
  contradict it: the invariant is **per `World`**, not "exactly one `World` in the process," and
  each of the client's and the server's `World`s independently keeps exactly one schedule and one
  accumulator. See the original decision's closing comment for the original wording and this document's own
  "Two `World`s, never one, and why" section above for why a second, wholly separate `World` was the
  right call rather than a violation of it.
