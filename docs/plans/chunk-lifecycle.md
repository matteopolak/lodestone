# Plan: the chunk lifecycle — tickets, status, unloading, async generation (#289, #292, #293, #297)

## What it is

The implementation plan for the four chunk-lifecycle issues (#289 ticket/status pipeline, #292
unloading and the save-on-unload hook, #293 non-blocking generation, #297 the spawn ticket), each
decomposed into agent-sized units with explicit file ownership, a named consumer, and a gate with a
negative control. Written 2026-08-04 against a verified tree; two of the four issue bodies contain
claims that are false against 26.2 or against the current tree, and those corrections are the first
section rather than a footnote.

## Verified current state (read this before trusting any issue body)

Everything in this section was re-grepped tree-wide with `/usr/bin/grep`, not read off an issue.

**The four issues are entirely unstarted.** `git log --grep '#289'`, `'#292'`, `'#293'`, `'#297'`
each return zero commits. Confirmed absent as identifiers anywhere under `crates/`: `ticket` (0
hits), `ChunkStatus` (0), `chunk_status` (0), no chunk-holder type (0), no distance-manager type
(0). `unload` has
168 hits but **only 3 in `lodestone-server`, all prose** (`protocol.rs`, `mobs/mod.rs`'s `surface_y` doc comment,
`scheduled_tick.rs`'s module doc); every real implementation is client-side (`lodestone-world/src/world.rs`'s
`World::unload`). `loaded_chunks` has 53 hits and **0 in `lodestone-server`**.

**There is no server-side chunk store of any kind.** `ChunkSource::column` (`chunk.rs`, impl on
`OverworldChunkSource`) returns an **owned `ChunkColumn` by value**, freshly generated on every call. The only
retention anywhere is `OverworldChunkSource::edits` (`chunk.rs`), a `Mutex<HashMap<(i32,i32),
ChunkColumn>>` populated **only** by `set_block` — an unedited column is regenerated on
every request, by design, and that design is documented at length in `OverworldChunkSource`'s own
doc comment. Two unrelated structures currently stand in for "which chunks matter":

- **`ViewTracker`** (`server.rs`), per connection, `loaded: HashSet<(i32,i32)>` —
  coordinates only, no data. `recenter` replaces the set wholesale, so a chunk that leaves
  and re-enters a view is fully regenerated and re-encoded.
- **`tick_area`** (`tick.rs`), a fixed `(RangeInclusive<i32>, RangeInclusive<i32>)` cloned from
  `mob_area` (`integrated.rs`). `tick.rs`'s own doc comment says so explicitly: *"not a generic 'loaded
  chunks' registry (this crate has none)"*.

**The tick loop regenerates terrain every tick.** `run_tick_loop` calls `world.column(cx, cz)`
(per due scheduled tick) and again inside the `tick_cz_range`/`tick_cx_range`
double loop, both in `tick.rs`. Its own doc comment concedes it: *"Every chunk in it is re-fetched via
`world.column(cx, cz)` **every tick**; for an unedited column this re-runs the generator"*
(`tick.rs`'s own doc comment). This is the concrete starting point for #289 and the single largest win
available from it.

**#293 is *not* what #414 already did, and #414's own text says so.** `275c765` ("perf(server): fan
out chunk generation over scoped threads", issue #414, **closed**) added
`generate_columns_parallel` (`chunk.rs`), used at exactly two call sites: `server.rs` (the
join burst) and `ViewTracker::build_batch` (`server.rs`, reached from `recenter`,
driven by the `PlayerMoved` arm). #414's body names itself *"a narrower, immediate
complement to #293 (full async/non-blocking `spawn_blocking` architecture)"*. **Parallel is a
throughput axis; non-blocking is a latency axis, and only the first was closed.** See the next
subsection for how much worse the latency axis is than #293's own body claims.

**The blocking is total, not partial — the runtime is single-threaded.**
`crates/lodestone-shell/src/net.rs`'s `run_async` builds the server's runtime with
`tokio::runtime::Builder::new_current_thread().enable_all().build()`, and everything runs inside
`runtime.block_on` — including `IntegratedServer::open_in_memory_with_mobs`, which is
what spawns `run_tick_loop`. So the serve task and the world tick task share **one thread**.
`generate_columns_parallel` ends in a `std::thread::scope` join (`chunk.rs`), which blocks the
calling thread until every worker finishes. On a current-thread runtime that blocks **every task in
the process** for the wall-clock of the batch: `serve_play`'s `tokio::select!` (`server.rs`) and
all four of its timers (keep-alive, time sync, vitals, container sync
timers, all in `server.rs`), *and* `run_tick_loop`'s `sleep_until` (`tick.rs`).

#293's body says this *"risks blocking the world tick too if generation and simulation ever share a
runtime thread pool without care."* They already share, in the strongest form available — one
thread — so the risk is not hypothetical and the word "risks" understates it. Every chunk-boundary
crossing in singleplayer today drops one or more 50 ms world ticks. This makes #293 the highest-value
unit in the cluster and it is the reason it is sequenced first below.

**Persistence: #298 and #300 are CLOSED; #437 is the open dependency.** `lodestone-anvil` landed
(`129f0bb`, `bbf27af`, `e8e76f5`) with a real `.mca` container and `level.dat` codec, verified
against region files this repo did not write. It has **zero consumers** — grepping
`lodestone-anvil`/`lodestone_anvil` across `crates/` matches only its own manifest, `src/`, and
`tests/`. It is a declared island. Two consequences for #292: the save-on-unload hook joins to
**#437**, not to #298; and `lodestone-anvil` models the **envelope only** (`region::build_region`,
`build_region_from_nbt`, `RegionFile::read_chunk_nbt_bytes`, `level_dat::{read,write}`) — the chunk
*schema* (the on-disk chunk NBT schema) is explicitly not in it and is #437's to decide.
**This plan does not design the region format or the chunk schema.** Note the 26.2 oracle's overworld
regions live at `world/dimensions/minecraft/overworld/region/`, not `world/region/`.

**`lodestone-world` is a dev-dependency only** (`crates/lodestone-server/Cargo.toml`), so the
`PalettedContainer`/`PackedArray`/`Arc<ChunkSection>` machinery is not on the server's dependency
path today. `docs/server-ecs.md`'s Dependencies section already sanctions promoting it.

**`run_tick_loop` has no extension point.** `tick.rs`'s `run_tick_loop` is `pub(crate) async fn` taking 8 fixed
concrete parameters with a hardcoded straight-line body — no callback list, no
trait-object collection, no schedule.

**CORRECTED 2026-08-04 — the LAN half of this entry is fixed and no longer applies.** It read:
*"Its only caller is `open_in_memory_with_mobs`; `IntegratedServer::bind` (`integrated.rs`) never
spawns it, so LAN worlds have no world tick at all."* That was true when written and was filed as
**#439, now closed**. `bind` spawns exactly one loop per world, outside the accept loop, gated by
`crates/lodestone-server/tests/lan_world_tick.rs` — whose load-bearing assertion is a *ratio*
(0-connection versus 2-connection tick rate) because the failure mode to fear is one loop **per
connection**, not zero. So units below that add per-tick work do **not** inherit a
singleplayer-only gap; they do inherit LAN's fixed `LAN_TICK_RADIUS = 2` tick area, which is what
this plan's ticket system replaces. See [`docs/server-tick-loop.md`](../dedicated-server.md).

**Redstone work is in flight in `tick.rs` and `random_tick.rs` right now** (~2,081 lines across five
untracked files, `tick.rs` +85, `random_tick.rs` +255). That is not migration work and must not be
mistaken for it. It is also the reason every `tick.rs` edit below is specified as a **named-anchor
insertion**, not a rewrite (per CLAUDE.md's never-rewrite-a-shared-file rule).

## The chunk-residency and generation-priority model, restated

Every fact below was verified by reading the real 26.2 generation server's own behaviour; only the
rule and its constants are reproduced here, never a source file or symbol name.

### Levels are the whole mechanism

A resident chunk carries a single integer **level** that simultaneously answers two questions: how
loaded it is, and how far generation must push it. Four thresholds partition it: level ≤ 31 means
fully simulating (entity ticking), ≤ 32 means block-ticking only, ≤ 33 means fully generated but
not yet ticking ("full"), and above a computed ceiling means the chunk should not be resident at
all.

| constant | value |
|---|---|
| full-chunk level | 33 |
| block-ticking level | 32 |
| entity-ticking level | 31 |
| the generation-dependency radius past full | computed from the pipeline's own neighbour
  requirements, not a literal — at least 8 |
| the loaded ceiling (`MAX_LEVEL`) | 33 + that radius |

**`level <= MAX_LEVEL` is the entire unload predicate.** A chunk's generation target is likewise
derived purely from its level: the distance a level sits past 33 maps directly to which generation
step that distance requires. So **the level number alone determines both the residency decision
and the generation target** — there is no second priority heuristic anywhere.

### Tickets carry a level; a minimum-spreading graph propagates it

A ticket is a `(timeout, flags)` pair, where the flags independently mark: persists across
restarts, keeps a chunk loading, keeps a chunk simulating, keeps the dimension itself active, and
can expire even while unloaded. The registered ticket categories:

| category | timeout (ticks) | flags | decoded |
|---|---|---|---|
| player-spawn | 20 | 2 | loading only |
| spawn-search | 1 | 2 | loading only |
| dragon-fight | 0 (none) | 6 | loading + simulation |
| player-loading | 0 | 2 | loading only |
| player-simulation | 0 | 12 | simulation + keep-dimension-active |
| forced (`/forceload`) | 0 | 15 | persist + loading + simulation + keep-dim |
| portal | 300 | 15 | as forced, but expiring |
| ender-pearl | 40 | 14 | loading + simulation + keep-dim |
| unknown/fallback | 1 | 18 | loading + can-expire-if-unloaded |

Adding a ticket "with radius" does **not** fan out to neighbours — it stores **one** ticket at the
centre chunk, at level `33 - radius`. The spreading is a separate minimum-fixed-point graph: each
neighbour's level is its source's level plus one per chebyshev step, so a chunk's effective level is
`min over all tickets t of (t.level + chebyshev_distance(t.chunk, this))`, and `radius` is expressed
purely as a level offset.

**There are two independent instances of that propagation graph, and the split is load-bearing:**
one fed only by tickets whose flags mark loading, one fed only by tickets whose flags mark
simulation. This is what lets a spawn ticket load a chunk **without making it tick**.

Tickets persist across restarts as part of the world's own saved state, serialising as
`{category, level, ticks_left}`. Expiry decrements `ticks_left` once per tick, and refuses to expire
a ticket whose chunk is not yet ready for saving unless that ticket is explicitly marked as allowed
to expire while unloaded.

### Unloading is deferred, save-gated, and budgeted

A chunk whose level crosses past the loaded ceiling moves into a pending-unload set rather than
being dropped immediately, and re-checks whether a save is in flight before actually dropping it,
re-arming itself if so — **a chunk is never dropped while a save is in flight.** Only once no save
is pending does it get marked unloaded, saved, and released. The drain itself is budgeted:
continues while time remains in the tick or while more than 2000 are queued, plus eager saves capped
at 20 per tick with an in-flight-write ceiling of 128 (its own 10-second per-chunk cooldown; the
routine saving budget is 200/tick).
**There is no fixed tick delay between "ticket dropped" and "chunk unloaded"** — the delay is
"whenever the save completes and the budget allows", which is a different shape from a timer
and should not be ported as one.

### Async generation, and where priority comes from

Generation and lighting each run through their own single-consumer background queue over a shared
executor, with a separate main-thread queue for the final promotion and send. Chunk NBT parsing and
writing happen off-thread; only the full-status promotion and the outgoing packet land back on the
main thread.

**Priority is the ticket level, not a separate score.** A generation request's priority is exactly
its ticket level, submitted straight into a four-band priority queue ordered by level. This is the
answer to #289's "loading-priority system": it is derived from the ticket graph and needs no new
heuristic and no new benchmark.

### The status pipeline

Generation runs through twelve ordered statuses — placing structure starts, resolving structure
references, biomes, noise shaping, surface rules, carving, feature placement, initializing light,
lighting, spawning, and finally "full" — the first eleven producing an intermediate ("proto")
representation and only the last a fully-realized one. Each status has its own neighbour
requirement: most steps need structure starts resolved out to a radius of 8, several need biomes at
radius 1, feature placement needs carving at radius 1 and can itself write into a neighbour at
radius 1 (features are the one step that writes across a chunk border), and lighting needs light
initialized at radius 1. A separate, cheaper pipeline applies when a chunk is restored from disk: it
skips every generation step and runs only structure-start loading, light initialization, lighting,
and the full-status finish.

### U7's premise is false for 26.2

**26.2 has no permanent spawn-chunk ticket.** Three independent confirmations:

1. The old configurable spawn-chunk-radius game rule is **deleted** by a data migration, and
   grepping the decompiled 26.2 source for any of its old names returns **that one deletion line
   and nothing else**.
2. World startup adds **no spawn ticket** of its own. It reactivates whatever tickets were already
   *persisted* from a previous session and waits for those to finish loading. On a brand-new world
   with no saved tickets, it loads nothing.
3. What actually loads spawn terrain is a **transient, loading-only** ticket: timeout 20 ticks,
   flags marking loading only — not simulating, not persisting. It is added at **radius 3** during
   the *configuration* phase of a player's join — before their entity even exists — and refreshed by
   a periodic keep-alive for as long as that phase lasts.

So #297's description — *"a configurable radius … held loaded by a permanent ticket independent of
any player being nearby, so redstone contraptions and farms near spawn keep ticking when everyone's
away"* — describes **pre-26.2** behaviour. And its stated verification, *"assert the spawn-radius
chunks remain in the loaded set and keep ticking with zero players connected,"* **would fail against
real 26.2**: a spawn ticket does not simulate and expires 20 ticks after the last refresh.
Writing that gate would encode a behaviour the real game does not have. Unit **U7** below
re-specifies #297 as the two things 26.2 actually does.

## Declared simplifications, and what each costs

Per the brief: these are simplifications, named as such, with the cost stated.

**S1 — one status transition, `Empty → Full`, not twelve.** `OverworldGenerator::column`
(`crates/lodestone-worldgen/src/overworld/mod.rs`, instrumented twin `column_timed`) is a
single monolithic per-column call that internally does noise, surface, carvers, aquifer and ore
features. There is no seam to stop it at `NOISE`. Costs, precisely:

- We cannot express "generated further because a neighbour needs it as a dependency." Therefore
  our `RADIUS_AROUND_FULL_CHUNK` is **0**, where the real generation pipeline computes this radius
  at runtime from its own neighbour requirements and it is at least 8, driven by the requirement
  that structure starts be resolved out to that distance. So our `MAX_LEVEL` is `33 + 0 = 33`, not
  the real pipeline's `33 + n`. **Do not port `MAX_LEVEL` as a literal 33 without this note
  attached**, or the next reader will "fix" it to match the real radius and silently widen
  residency by 8 rings.
- A one-block write radius for feature placement is unrepresentable, so features cannot write
  across a chunk border. This is a **pre-existing** `lodestone-worldgen` limitation, not one this
  plan introduces — worth stating so nobody attributes border-truncated trees to the ticket
  system.
- The distinction between "generate everything" and "restored from disk, skip generation, still
  run light" collapses to "present or absent". Fine until #437 lands; revisit then.

**S2 — no future-graph of per-status holders.** With one transition we need one in-flight future
per chunk rather than the real pipeline's per-status future-dependency joins across many
intermediate holder objects. A `HashMap<(i32,i32), Shared<…>>` of in-flight generations suffices.
**The dedup property still matters and must be kept**: two connections (or a connection and the
tick loop) requesting the same ungenerated chunk must produce **one** generation, not two.

**S3 — NOT simplified: keep both trackers.** It is tempting to collapse the loading-level graph and
the simulation-level graph into one. Do not. The two-instance version is the *same* propagator
run twice with a different ticket filter (`doesLoad()` vs `doesSimulate()`) — roughly 40 extra lines
— and collapsing it destroys exactly the distinction #297 and #292 both depend on: a chunk that is
resident but must not tick. If they were collapsed, `PLAYER_SPAWN` chunks would tick, which is the
bug #297's own (stale) verification would have baked in.

**S4 — no per-chunk save budget initially.** The real game's routine-save budget of 200/tick,
eager-save budget of 20/tick, and in-flight-write ceiling of 128 exist to keep saving off the tick
budget. U6 lands the save *hook* with an unbudgeted synchronous call behind #437, and the budget is
a follow-up. Cost: a mass unload (a player teleporting far away) will spike MSPT until the budget
exists. Named in Risks.

## Where the store lives: the server-ECS question

**These four issues are not blocked behind any bevy migration phase.** The reasoning, since the brief
asks for it explicitly:

`docs/server-ecs.md` is a *decision record* — its own opening says **"nothing in
`crates/lodestone-server/` implements this yet."** The migration's subject is where *packet apply* and
*plugin adjudication* happen (its "The adjudication window is the point" section, citing the existing straddle in
`server.rs` its own "The straddle already exists, with no ECS involved" section names). Chunk residency is a different axis: the real game's own
chunk-residency map is not an ECS structure either — it is a plain hash map of holder objects keyed
by chunk position. Chunks are not entities.

So: build `ChunkStore` and `TicketStore` as **plain structs behind an `Arc` handle wrapper**, in the
exact shape this crate already uses four times — `BlockTickFeed(Arc<Mutex<Vec<…>>>)` (`tick.rs`),
`ExplosionFeed` (`tick.rs`), `MobHandle`, `BlockEntityHandle`. This is forward-compatible **by
construction**: a `bevy_ecs::Resource` is just a struct the `World` owns, so the migration phase that
wants it inserts `world.insert_resource(store)` and deletes the wrapper. No redesign.

Two constraints that follow, and they are the real design content:

1. **The store cannot be lock-free the way `docs/server-ecs.md` promises for the `World`.** That
   promise holds because "every connection task's job is to enqueue proposals and read published
   snapshots, never to reach into the `World` directly." But a connection task must read chunk
   *data* to encode `level_chunk_with_light`. So either the store is `Arc`-shared with interior
   locking (today's arrangement, and what U3 does), or chunks are published as snapshots (the
   migration's end state). Pick the first now, and make the boundary crossing a **handle, not a
   copy**, so the eventual switch changes *who holds the lock* rather than *what crosses*.
2. **Therefore the read API returns section handles, not columns.** This is what makes constraint 1
   cheap later and what respects the prior art below.

## Memory: the risk this plan's own success creates

This is the largest technical risk in the cluster and it is created by fixing #289, not by leaving it
broken.

Today nothing retains a column, so `ChunkColumn`'s representation is free. It is
`palette: Vec<String>` + **`blocks: Vec<u16>` dense over the full world height** (`chunk.rs`'s
`ChunkColumn::blocks` field, index `(y_local * 16 + z) * 16 + x`). Arithmetic on `size_of`, **not a
measurement** — U2 exists to measure it:

| | columns | dense `blocks` at 384 rows |
|---|---|---|
| per column | 1 | 16 × 384 × 16 × 2 B ≈ **192 KiB** |
| RD 8 | 289 | ≈ 54 MiB |
| RD 16 | 1089 | ≈ 204 MiB |
| RD 32 | 4225 | ≈ 792 MiB |

A residency system turns a free representation into that. The prior art that solves it already
exists and is **not on the server's dependency path**: `lodestone-world`'s `PalettedContainer`
(`container.rs`, `Storage::{Single, Indirect, Direct}` in the same file) over `PackedArray`
(`packed.rs`) at real bits-per-entry, plus **`Arc<ChunkSection>` copy-on-write section sharing** —
`ChunkColumn.sections: Vec<Option<Arc<ChunkSection>>>` (`column.rs`), `World::section` returning
`Option<Arc<ChunkSection>>` (`world.rs`), whose doc says it *"bumps a section refcount rather
than copying… A later edit of that section forks it copy-on-write."* `PackedArray::from_longs`
(`packed.rs`) is the designated pool-intake seam. `docs/chunk-memory-pool-footprint.md` measured
the real distribution over 4225 columns / 101,400 sections: block states only ever hit
`bits_per_entry ∈ {0,4,5,6,7,8,15}`, biomes `{0,1,2,3,6}` — about **7 real size classes, not 14**.

**The rule this imposes on every unit below: the ticket system must never clone section data.**
`OverworldChunkSource::column` currently `clone()`s an entire edited column on every read
(`chunk.rs`) — U3 must delete that, not inherit it. Minimum bar is `Arc<ChunkColumn>`
(kills the clone and the per-tick regeneration); the sectioned representation is **U8**, gated on
U2's measured number rather than on this table's arithmetic.

## Units

Ownership is exclusive per unit unless stated. `lodestone-server/src/lib.rs` is a choke point
brokered through the orchestrator — every patch to it is given as an exact insertion below, never a
rewrite. Same for `tick.rs`, which has ~85 lines of in-flight redstone work in it.

---

### U1 — non-blocking generation. First, and independently valuable.

**Owns:** `crates/lodestone-server/src/chunk.rs`, `crates/lodestone-server/src/server.rs`.
**Touches (broker):** `integrated.rs` (call sites), `lib.rs::server::serve_connection` (re-export).

**What.** Add `generate_columns_offloaded(source: Arc<S>, coords: Vec<(i32,i32)>) -> Vec<ChunkColumn>`
in `chunk.rs` beside `generate_columns_parallel`: an `async fn` that wraps the existing scoped fan-out
in `tokio::task::spawn_blocking` and `.await`s it. Replace both call sites (`server.rs`,
`server.rs`).

**CORRECTED 2026-08-04 — this paragraph's central claim was wrong, and #293 landed without it.**
It read: *"The signature change is the entire cost, and it is unavoidable… `serve_connection` is
publicly re-exported (`lib.rs::server::serve_connection`), so this is a **public API change** —
that is the broker item."*

The `'static` requirement is real; the conclusion that it forces a public signature change is not.
`server.rs` gained a private `SourceRef<'a, S>` enum — `Borrowed(&'a S)` / `Shared(&'a Arc<S>)` —
threaded through the private dispatch chain, so both shapes share one `serve_connection_inner` body
with no duplication. Two consequences the plan did not anticipate:

- **`mod server` is private (`lib.rs::server`) and `lib.rs::server::serve_connection` re-exports
  only the *name* `serve_connection`, not any type.** So the new `serve_connection_shared` /
  `serve_connection_with_mob_events_shared` entry points are `pub(crate)`, and #293 required **no
  `lib.rs` patch and no public API change at all**. The brokered choke point was never touched, and
  no `crates/protocol/v770/tests/*` call site changed. The only public-surface change is a widened
  bound (`S: ChunkSource` → `+ 'static`), which every real implementor already satisfied.
- **The `Borrowed` arm turned out to be an asset, not debt.** It is #293's permanent negative control
  — `chunk.rs`'s gate drives the blocking path as its second arm and requires it to starve a timer
  task completely (measured: 0 ticks versus 21 over the same ~280 ms).

The `Arc` plumbing in `integrated.rs` is as the plan described: `open_in_memory_with_mobs_using`'s
`conn_source` was already
`Arc::clone(&source)` and only needed `&conn_source` instead of `&*conn_source`, while
`open_in_memory_with_entities` needed an `Arc::new`.

The plan's `block_in_place` rejection was **correct and is confirmed empirically** rather than from
the docs: on a `new_current_thread` runtime it panics with `can call blocking only when running on
the multi-threaded runtime`, while `spawn_blocking` returns `Ok` and lets a 10 ms timer task tick 25
times during a 300 ms blocking call (0 times inline).

**Reject `tokio::task::block_in_place`**, which needs no signature change and looks cheaper. It
**panics on a current-thread runtime**, and the production runtime is current-thread
(`net.rs`), as is every server test (19 × `#[tokio::test(start_paused = true)]`). It would
panic in singleplayer. `spawn_blocking` is fine on a current-thread runtime — the blocking pool is
separate from the core thread.

**Consumer / driver.** Unchanged consumers, same two call sites: `server.rs` inside the
`ConfigurationFinished` arm, and `server.rs` inside `build_batch`, reached from `recenter`
 from the `PlayerMoved` arm. No island risk — this is a swap at live call sites.

**Gate.** `crates/lodestone-server/tests/generation_does_not_stall_the_runtime.rs` (new file, owned
by this unit). On a **real current-thread runtime** (assert the flavour — see vacuity below), spawn a
task that increments an `Arc<AtomicU64>` every 50 ms via `tokio::time::sleep_until`, mimicking
`run_tick_loop`'s shape (`tick.rs`). Run an RD-6-or-larger join through the new offloaded path.
Assert the counter advanced by **≥ N** during the burst, where N is derived from the measured burst
wall-clock, not hardcoded.

**Negative control that must fail.** The same test body calling `generate_columns_parallel`
directly (it stays in the tree, so the control is permanent, not a temporary neuter). The counter
must advance by **≈0**. Run it and watch it fail before believing the positive arm.

**What would make it vacuous.** *A multi-thread runtime.* Under `#[tokio::test(flavor =
"multi_thread")]` a second worker polls the timer and **the control passes too**, so the gate
measures nothing. The flavour assertion is therefore the load-bearing line, not decoration. Second
vacuity: asserting on `TickStats`/`TickClock` (`tick.rs`) instead of a local counter — see the
duration note below.

**Duration species — does a server counter outlive this gate?** Yes, and it is a trap here.
`TickClock` (`tick.rs`) accumulates MSPT/TPS/overrun over the whole server lifetime in atomics
plus a `Mutex<VecDeque<u64>>`. A gate that reads `tick_stats()` absolutely cannot distinguish "no
stall now" from "the join stall already averaged away." **Assert on a delta bracketing the operation,
or on a fresh local counter.** Prefer the local counter — it has no lifetime at all.

---

### U2 — measure the retained-column cost (the missing RSS half). Parallel with U1.

**Owns:** `crates/lodestone-server/examples/bench_worldgen.rs`.

**What.** #87 asks for peak memory alongside the existing wall-clock. Verified already present in
that file: the RD sweep via argv (`radius`, parsed in `main`), the parallel wall-clock and speedup
, and a thread-count/efficiency sweep, all recorded into the gitignored
`bench-results/generation.jsonl`. **Missing is exactly peak RSS.** Add: a mode that *retains* every
generated column versus one that drops it, reporting `/usr/bin/time -l` max RSS for each (reuse
`lodestone-allocbench`'s proven macOS approach per #87's own instruction — do not invent a second RSS
method), plus a derived per-column retained-bytes figure from `blocks.capacity()` and the palette.

**#85 is substantially already done and should not be re-planned here.**
`OverworldGenerator::column_timed` **exists** (`crates/lodestone-worldgen/src/overworld/mod.rs`,
`StageTimes` in `overworld/output.rs`) — #85's body correctly told the reader to check before assuming it needed
recreating, and it did not. The stage split is already recorded, by `bench_stage_split` in
`crates/lodestone-worldgen/benches/generation.rs` as `stage_shape_pct` /
`stage_fluid_heightmap_pct` / `stage_surface_pct` / `stage_intern_pct` — note those are
**four** stages, so #85's remaining ask (carvers, aquifers and ore features as *separate* line items)
is genuinely still open, and this plan does not close it. **Do not move either bench** —
`bench_worldgen.rs`
lives in `lodestone-server/examples/`, not `lodestone-worldgen/`, which is correct: it benches the
server's consumer of the generator, which is precisely what these units change.

**#289's priority system needs no new bench.** Priority *is* the ticket level
(`ChunkTaskDispatcher.java`), a derived integer, so its correctness is a unit-testable ordering
property (U4's gate), not a throughput question.

**Gate.** None — this is a measurement tool, not a test, and #87 is labelled `bench`. But the
retained-vs-dropped pair **is** its own control: if the RSS delta between the two modes is ≈0, the
measurement is broken (columns are being dropped in both arms, or the allocator is not returning
pages), and the run must be treated as a failure to measure rather than as "residency is free."

**Debug timings are not evidence.** An ore sweep measured 700 s in debug. Every number from this
unit must come from `--release`.

---

### U3 — `ChunkStore`: residency and a one-step status, no tickets yet (part 1)

**Owns:** `crates/lodestone-server/src/chunk_store.rs` (new).
**Broker patches:** `lib.rs` (+1 `mod`, +1 `pub use`), `tick.rs` (two anchored edits), `server.rs`
(read path).

**What.** `ChunkStatus { Empty, Full }` (S1) and:

```
struct ChunkEntry { column: Arc<ChunkColumn>, status: ChunkStatus, level: i32 }
struct ChunkStore { entries: HashMap<(i32,i32), ChunkEntry>, in_flight: HashMap<(i32,i32), …> }
pub struct ChunkStoreHandle(Arc<Mutex<ChunkStore>>);   // same shape as BlockTickFeed, tick.rs:113
```

Read API hands back `Arc<ChunkColumn>` — **never a clone** (see the memory section). `in_flight`
provides S2's dedup.

**Consumer / driver — this is the island question, answered per call site.** A new store with nothing
reading it is precisely rule 1's dominant defect, so all three consumers land *in this unit*:

1. **`tick.rs`** — `let mut column = world.column(cx, cz);` inside the random-tick double loop
   becomes a store read. This is the win: it deletes a full generator run per chunk per
   tick (`tick.rs`).
2. **`tick.rs`** — the same substitution in the scheduled-block-tick drain.
3. **`server.rs`** — `generate_columns_offloaded` (U1) writes its results **into the
   store** and `ViewTracker` encodes from store reads.

Writes (`world.set_block` calls in `tick.rs`) go through the store, which keeps
`OverworldChunkSource::edits` (`chunk.rs`) as the persistence layer beneath it until #437.

**`tick.rs` is under concurrent redstone edits.** Both edits are single-line substitutions at named
anchors. Re-read the file immediately before writing, and commit with the pathspec form.

**Exact `lib.rs` patch (broker):** insert `mod chunk_store;` between `mod chunk;` and
`mod composter;`; insert `pub use chunk_store::{ChunkStatus, ChunkStore, ChunkStoreHandle};`
immediately after the `pub use chunk::{…}` line.

**Gate.** `crates/lodestone-server/tests/chunk_store.rs`: drive `run_tick_loop`'s random-tick path
over a fixed `tick_area` for K ticks against an instrumented `ChunkSource` that counts `column()`
calls. Assert the count is **the number of distinct chunks, not chunks × K**.

**Negative control that must fail.** The same assertion with the store bypassed (call `world.column`
directly, i.e. today's code) must report chunks × K. Available permanently, since `ChunkSource` keeps
that method.

**What would make it vacuous — and this is the repo's own documented trap.**
`OverworldGenerator` carries a **pre-ore memoisation cache** (per-instance, keyed on exact
`(cx, cz)`, capped at 512, never evicted below that). A generation-count gate built on
`overworld_chunk_source` will pass **even with a totally broken store**, because the cache absorbs
the second call. This exact vacuity was found and fixed in
`chunk.rs`'s `parallel_generation_is_deterministic_and_matches_serial`, whose repair was to
construct a **fresh, independent source for every arm**. Do the same: count on a *hand-written*
counting `ChunkSource`, never on the real generator.

**Duration species.** The call counter must be created inside the gate and read as a delta across a
known tick count. Do not read `TickClock` — it outlives the gate (see U1).

---

### U4 — `TicketStore` and the level propagator (part 2)

**Owns:** `crates/lodestone-server/src/ticket.rs` (new).
**Broker patches:** `lib.rs` (+1 `mod`, +1 `pub use`), `tick.rs` (one anchored insertion),
`chunk_store.rs` (U3's file — sequence after U3, same owner if possible).

**What.** Port, with S1's `MAX_LEVEL = 33` and its caveat written into the doc comment:

- `TicketType { timeout: u64, flags: u8 }` with the five flag constants and the nine registered
  types, transcribed from `TicketType.java` (the table above). Predicate methods `persist()`,
  `does_load()`, `does_simulate()` mirroring the same file.
- `Ticket { ty, level, ticks_left }` (`Ticket.java`), `reset_ticks_left`.
- `add_ticket_with_radius(ty, pos, radius)` → one ticket at level `33 - radius`
  (`TicketStorage.java`). **Not a fan-out.**
- The propagator: effective level = `min over tickets t of (t.level + chebyshev(t.pos, pos))`,
  from `ChunkTracker.computeLevelFromNeighbor == fromLevel + 1` (`ChunkTracker.java`). A
  straightforward BFS from ticket sources is correct and clearer than porting
  `DynamicGraphMinFixedPoint`; note in the doc comment that vanilla's incremental version exists for
  a scale we do not have yet.
- **Two instances** (S3): loading (filter `does_load()`) and simulation (filter `does_simulate()`).
- `purge_stale()` — one `decrease_ticks_left()` per tick, per `TicketStorage.java`.

**Consumer / driver.** `run_tick_loop` gains one call per tick: `tickets.purge_stale()` then
`propagate()`, then push the resulting levels into `ChunkStore`. **Anchor: immediately after the
`fluid_ticks.drain_due` loop closes in `tick.rs`, before the random-tick comment there** — matching vanilla's own ordering (`ServerChunkCache` runs after `ServerLevel`'s
`blockTicks`/`fluidTicks`, already cited in that comment). `ChunkStore`'s status target is now
`generationStatus(level)` (S1: `Full` iff `level <= 33`), and the simulation level decides whether the
random-tick loop visits a chunk at all — replacing the fixed `tick_area` (`tick.rs`).

**Gate.** `crates/lodestone-server/tests/ticket_levels.rs`, three properties, each with an expected
value derived from the vanilla constants above rather than from our own code:

1. A `PLAYER_SIMULATION` ticket at level 31 on `(0,0)` gives `(0,0)` level 31, `(3,0)` level 34,
   `(0,-2)` level 33 — i.e. Chebyshev, and `(3,0)` is **not loaded** under `MAX_LEVEL = 33`.
2. Two tickets ⇒ minimum wins, not sum, not last-write.
3. **The S3 property**: a `PLAYER_SPAWN` ticket (loading-only, flags 2) makes its chunk *resident*
   and **not** simulating. This is the assertion that fails if anyone collapses the two trackers.
4. Ordering: chunks sort by level, so nearer-a-player dequeues first (#289's "priority", derived).

**Negative control that must fail.** A one-tracker build (feed both graphs from all tickets
unfiltered) must fail property 3 and only property 3. Construct it in the test as a second
configuration, so the control is permanent.

**What would make it vacuous.** Deriving the expected levels by calling our own propagator (the
`decode(encode(x))` trap). Every expected number in properties 1–3 must be written out by hand from
`ChunkLevel.java` and `TicketStorage.java`.

**Duration species.** `purge_stale` decrements per tick, so a gate asserting "the 20-tick
`PLAYER_SPAWN` ticket expired" must count ticks it drives itself. `run_tick_loop`'s `game_tick`
(`tick.rs`) is loop-local and monotonic from loop start, so a **delta** on it is safe; an
absolute value is not, since another test's loop may have advanced it.

---

### U5 — player tickets replace `ViewTracker`'s residency role (part 3, the island-closer)

**Owns:** `crates/lodestone-server/src/server.rs`.

**What.** `ViewTracker` currently conflates two jobs. Split them along
`docs/server-ecs.md`'s never-straddle line:

- **Replication (stays):** which chunks *this connection* has been sent; `loaded`,
  `build_batch`, the forget-chunk diff, the one-in-flight batch gate
  (`send_view_update`).
- **Simulation (moves to tickets):** *which chunks exist*. The `PlayerMoved` arm adds/moves
  a `PLAYER_SIMULATION` ticket at level 31 (`DistanceManager.java`) plus a
  `PLAYER_LOADING` ticket at `PLAYER_TICKET_LEVEL`, instead of driving generation itself.
  `ClientInformationChanged` adjusts the ticket radius rather than calling
  `set_view_radius` to generate.

Vanilla's own relationship, for the constant: `getPlayerTicketLevel()` is
`max(0, byStatus(ENTITY_TICKING) - simulationDistance)` (`DistanceManager.java`) with
`simulationDistance = 10`, and `PlayerTicketTracker(32)` is the separate view-distance
graph.

**Why this unit is mandatory and not optional polish.** Without it, U3 and U4 are a textbook island:
a store and a ticket graph that nothing populates, while `ViewTracker` goes on generating chunks
independently at `server.rs`. **U3+U4 without U5 must be treated as unlanded.**

**Gate.** `crates/lodestone-server/tests/serve_play.rs` (existing file — *append*, do not rewrite;
it is shared). Two connections at overlapping positions: assert each chunk in the overlap is
generated **once** (counting `ChunkSource`, per U3's counter), and that a chunk in one player's view
only stays resident when the *other* player moves away. Then: a chunk with **zero** tickets is not
resident.

**Negative control that must fail.** The pre-U5 path (`ViewTracker` generating directly) must show
the overlap generated **twice**. Since U1/U3 change those call sites, capture this control as a
measurement *before* landing U5 and record the number in the commit message — a control you cannot
re-run later is worth less, so prefer keeping a `#[cfg(test)]`-only direct-generation path if it can
be done without a second production code path.

**What would make it vacuous.** The generator's 512-entry memo cache again (see U3) — one fresh
source per arm. Also: if the two connections' views do not actually overlap, the gate passes
trivially. Assert the overlap is non-empty first, as its own precondition check that *fails* rather
than skips.

---

### U6 — unloading and the save-on-unload hook

**Owns:** `crates/lodestone-server/src/chunk_store.rs` (U3's file; sequence after U3/U4).
**Blocked on:** #437 for the save half only.

**What.** Two separable halves — land them separately, because only one is blocked:

- **Drop (unblocked).** When a chunk's loading level exceeds `MAX_LEVEL` — a chunk is loaded iff
  `level <= MAX_LEVEL` — move it to a `pending_unloads` map and drain it. Keep the real game's
  **re-arm** property: never drop while a save is in flight.
- **Save (blocked on #437).** Behind a trait so the drop half lands now:
  ```
  trait ChunkSink { fn save(&self, cx: i32, cz: i32, column: &ChunkColumn) -> Result<()>; }
  ```
  Default impl is a no-op that **logs at `warn`**, not silently — #292's own trap is that "unloading
  with nowhere to save to just means silent data loss, which is worse than the current never
  unloads." #437 supplies the real impl over
  `lodestone_anvil::region::build_region_from_nbt`. **Do not design the format or the chunk schema
  here** (see the Persistence note above; the schema is #437's decision).
  Because the sink is a no-op until #437, U6's drop half must **refuse to drop an edited column** —
  gate on `OverworldChunkSource::edits` (`chunk.rs`) membership. An unedited column is
  regenerable from the seed, so dropping it is lossless; an edited one is not. That single condition
  is what makes landing the drop half before #437 safe rather than reckless.

**Consumer / driver.** The same per-tick call U4 adds after `tick.rs`, extended with a
`process_unloads()` step.

**Gate.** `crates/lodestone-server/tests/chunk_unload.rs`: drive a player ticket outward, assert the
resident count **drops**, and assert an **edited** column is never dropped while the sink is a no-op.

**Negative control that must fail.** With the unload step disabled, the resident count must grow
monotonically — i.e. reproduce today's behaviour and watch the assertion fail.

**What would make it vacuous — the sharpest duration trap in the cluster.** "Assert the in-memory
chunk count drops" (#292's own suggested verification) is meaningless against a counter that
accumulates past the gate. Assert on **`ChunkStore::len()` sampled before and after**, and *also*
assert the intermediate peak, or a store that never grew in the first place passes. And do **not**
use process RSS as the instrument: the allocator does not return pages promptly, so RSS can stay
flat across a correct unload — that is U2's tool, on a different question.

---

### U7 — the spawn ticket, re-specified for 26.2

**Owns:** `crates/lodestone-server/src/ticket.rs` (U4's file), plus the join path in `server.rs`.
**Blocked on:** U4, and on `docs/plans/world-state.md`'s unit **P1** (#329, world spawn point) — you
cannot ticket a spawn you cannot locate. #297 correctly warns that
`crates/lodestone-server/src/spawn.rs` is the native-vs-wasm task-spawning seam, **not** world spawn
logic; confirmed (3.3 KB, no spawn-point code). Do not lose time there.

**What — and note this is not what the issue says.** Per the citations above, implement the two
things 26.2 actually has:

1. **`PLAYER_SPAWN`: transient, radius 3, loading-only.** Timeout **20** ticks, flags **2**,
   added at the **configuration** phase — before the player entity exists — and refreshed by a
   periodic keep-alive for as long as that phase lasts. Purpose is
   "the joining player has terrain to join into", nothing more. Anchor: the
   `ConfigurationFinished` arm at `server.rs`, where the join burst already lives.
2. **`FORCED`: persistent, simulating, level 31.** The forced-ticket level equals the
   entity-ticking level (31), timeout 0, flags 15. This — **not** a spawn
   radius — is the current game's "keep loaded and ticking with nobody nearby", and it comes from
   `/forceload`. Persisting it needs the same saved-ticket storage described above, so the
   *persistence* is #437's; the in-memory ticket type and its level are U7's.

**Do not implement a spawn-chunk-radius setting.** It is deleted by a data migration in the
current game and appears nowhere else in it.

**Gate.** `crates/lodestone-server/tests/spawn_ticket.rs`, asserting the **26.2** behaviour, which is
the opposite of #297's suggested gate on the key point:

1. At configuration-finished, chunks within Chebyshev 3 of spawn are resident. Radius 3 ⇒ level
   `33 - 3 = 30` at centre, so the ring at distance 3 is level 33 — resident — and distance 4 is 34,
   **not** resident. That off-by-one is derived from `TicketStorage.java` and is exactly what
   a hand-rolled version gets wrong.
2. Those chunks are **not simulating** (loading-only, flags 2).
3. With no refresh, the ticket **expires after 20 ticks** and they become droppable.
4. A `FORCED` ticket at level 31 **does** simulate, with zero players connected.

**Negative control that must fail.** Property 2 must fail under a one-tracker build (U4's control,
reused). Property 3 must fail if `purge_stale` is not driven — assert the tick count you drove.

**What would make it vacuous.** Writing #297's own gate instead of this one. *"Assert the
spawn-radius chunks remain in the loaded set and keep ticking with zero players connected"* is
**false for 26.2** — a `PLAYER_SPAWN` ticket neither simulates nor survives 20 ticks. That gate would
pass only against an incorrect implementation, which is the worst available outcome: a green test
locking in wrong behaviour. Property 4 is where "keeps ticking with nobody nearby" belongs.

---

### U8 — sectioned storage (conditional on U2's measurement)

**Owns:** `crates/lodestone-server/src/chunk_store.rs`, `crates/lodestone-server/Cargo.toml`.

**What.** Promote `lodestone-world` from dev-dependency (`Cargo.toml`) to a real dependency —
sanctioned by `docs/server-ecs.md`'s Dependencies section, and version-free, so
`cargo xtask check-isolation` stays clean. Store sections as `Arc<ChunkSection>` per
`lodestone-world/src/column.rs` and `world.rs`, so a connection encode bumps a refcount
instead of copying, and an edit forks copy-on-write.

**Trigger, not a schedule.** Land this if U2 measures retained-column cost near the arithmetic in the
memory section (≈192 KiB/column). Skip it if U2 shows the real figure is far lower. **Do not decide
from the table above** — it is `size_of` arithmetic, not a measurement, which is the whole reason U2
exists.

**Gate.** Reuse `lodestone-world/tests/pool_footprint.rs`'s `derive_size_classes` approach:
assert the server's resident set at RD 8 fits a stated byte budget, with the budget derived from
`docs/chunk-memory-pool-footprint.md`'s measured `bits_per_entry ∈ {0,4,5,6,7,8,15}` rather than
guessed. **Negative control:** the dense `Vec<u16>` representation must **exceed** that budget — so
the gate distinguishes the two representations and cannot pass on either.

## Order, and the real blockers

```
U1 (#293)  ──┐                      independent, first, fixes a live total-runtime stall
U2 (#87)   ──┤  parallel            independent measurement; gates U8
             ↓
U3 (#289a) ──→ U4 (#289b) ──→ U5 (#289c)      U5 is mandatory, not polish
                               ↓
                        ┌──────┴──────┐
                     U6 (#292)     U7 (#297)
                        ↓              ↓
                     #437 for       P1 (#329) for the spawn point,
                     the save half  #437 for FORCED persistence
                                    U8, only if U2 says so
```

**Real blockers, distinguished from sequencing preferences:**

- **The open persistence-wiring dependency blocks half of U6 and part of U7.** Genuine, external, and
  already unblocked on its own dependencies (the anvil codec work is closed). U6's drop half routes
  around it via the edited-column refusal.
- **`docs/plans/world-state.md`'s P1 blocks U7.** Genuine cross-plan dependency.
- **U5 blocks nothing but is blocked *by* nothing either** — and shipping U3+U4 without it produces
  an island. Treat U3–U5 as one deliverable with three commits.
- **The net-thread/game-thread split design task blocks nothing here.** See the verdict below.
- **No bevy-migration phase blocks anything here.** See "Where the store lives".
- ~~**LAN (`IntegratedServer::bind`, `integrated.rs`) spawns no tick loop**, so every unit that
  adds per-tick work is singleplayer-only until that is fixed.~~ **Fixed — #439 is closed.** `bind`
  now spawns exactly one loop per world. Per-tick work added by the units below reaches LAN as well
  as singleplayer.

### U1 and the net-thread/game-thread split design task: not a dependency, in either direction

**Verdict: U1 can and should land before that design task.** Evidence:

1. **The design task asks for a design, not code.** Its own scope: *"design a net-thread/game-thread
   (or async-task/tick) split for `lodestone-server` once multi-connection support is on the table —
   **no code changes needed until then**"*. Its second half (bounding the shell's relay channel) is
   client-side and touches nothing in this cluster. Note that task's own citation for that channel
   has **drifted** — those lines are now a doc comment about sky defaults; the
   unbounded `std::sync::mpsc::channel()` pair is in `connect_impl` (`crates/lodestone-shell/src/net.rs`). A
   §2 instance in miniature, and a reason to name the current symbol rather than quote an old citation forward.
2. **The split U1 needs already exists.** `run_tick_loop` is its own tokio task
   (`integrated.rs`) communicating with connections through `Arc<Mutex<…>>` feeds
   (`BlockTickFeed`, `tick.rs`; `ExplosionFeed`, same file). What the design task is really about is that
   *packet apply* still happens inline on the connection task — documented independently in
   `docs/server-ecs.md`'s "The straddle already exists, with no ECS involved" section (`server.rs`).
   That is an adjudication concern, orthogonal to where generation runs.
3. **U1's fix is `spawn_blocking` + a signature change, and needs no split.** `spawn_blocking` works
   on the current-thread runtime the shell actually builds (`net.rs`) because the blocking pool
   is separate from the core thread.
4. **The dependency runs the other way, weakly.** The eventual thread split would be *easier*
   after U1, because U1 removes the one place a connection task blocks the whole runtime.
5. **Nothing U1 does becomes wrong under that later split.** `spawn_blocking` is correct on both a
   current-thread and a multi-thread runtime; a later split does not invalidate it.

The one honest cost of ordering U1 first: its *call-site arrangement* in `server.rs` is
rewritten by U3/U5. The **signature change** (`&S` → `Arc<S>`) is not — the store needs it too — and
the **gate** is not: "the world tick keeps advancing during generation" is exactly the property U3–U5
must not regress, so the test outlives the patch. Landing U1 first buys a live stutter fix and a
permanent gate at the price of ~30 lines of call-site churn.

## Top risks

1. **Residency turns a free representation into a ~200 MiB one, and the tests will not notice.**
   `ChunkColumn` is a dense `Vec<u16>` over full world height (`chunk.rs`); nothing retains
   it today, so nothing pays. Every unit from U3 on retains it. A functional gate on ticket levels
   passes at any memory cost, so **only U2's measurement can see this** — which is why U2 is
   sequenced parallel with U1 rather than last, and why U8 exists as a pre-planned conditional
   rather than a future surprise. Secondary form: `OverworldChunkSource::column` clones a whole
   column per read (`chunk.rs`); U3 must delete that path, not build on it.

2. **`tick.rs` and `server.rs` are the two most contended files in this cluster, and three units
   want the same anchors.** `tick.rs` has ~85 lines of in-flight redstone work; `server.rs` is
   110 KB and holds `ViewTracker`, all four connection timers, and the packet dispatcher. U3, U4 and
   U6 all insert into `run_tick_loop`'s body near `tick.rs`, and U1 and U5 both edit the
   `PlayerMoved` arm (`server.rs`). Mitigation: single owner for U3/U4/U6 (they share
   `chunk_store.rs` and `ticket.rs` anyway), every `tick.rs` edit an anchored insertion rather than a
   rewrite, re-read immediately before writing, pathspec-form commits. **The failure mode is not a
   merge conflict — it is a silent wholesale overwrite**, which is how three edits to `sim.rs` were
   destroyed once already.

3. **A green gate that locks in wrong behaviour, because two issue bodies are stale.** #297's
   suggested verification is false against 26.2 on its central claim (a `PLAYER_SPAWN` ticket neither
   simulates nor outlives 20 ticks), and #293's body understates its own severity by describing a
   single-threaded runtime's guaranteed total stall as a "risk" contingent on sharing a thread pool.
   An agent implementing either issue from its body alone produces something that passes its own test
   and is wrong. Compounding it, the generator's 512-entry memo cache makes **every**
   generation-count gate in this plan vacuous unless each arm constructs a fresh source — the exact
   trap already found and fixed once in `chunk.rs`'s own determinism test. Mitigation: the
   per-unit "what would make it vacuous" paragraphs are not commentary, and every expected constant
   above is transcribed from a cited jar line rather than derived from our own code.
