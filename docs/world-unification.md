# One bevy `World` — §4.1(c)

## What it is

Until this change the process held **three** `bevy_ecs::World`s: the net thread's
(`lodestone_client::state::SharedState`, authoritative over the network read-model), the entity
interpolator's (`lodestone_shell::entities::EntityInterpolator`), and the driver's
(`lodestone_shell::sim::Sim`). It now holds **one**, behind
`lodestone_ecs::EcsHandle = Arc<parking_lot::RwLock<World>>`, and that one `World` carries **one**
`GameTick` schedule driven by **one** 20 Hz accumulator.

This is clause **(c)** of [`bevy-migration.md`](./bevy-migration.md) §4.1. Clause (d) — the *chunk*
store — landed in Stage 4 and is a different thing; see
[`chunk-world-resource.md`](./chunk-world-resource.md), which is where the two clauses were first
disentangled.

Five things were blocked on this and nothing else. Four are now done:

| blocked on (c) | status |
|---|---|
| the two-clock divergence (below) | **fixed** — one accumulator, one clamp |
| `CorePlugin`'s refusal to insert `WorldTime` | **retired** — it inserts `WorldTime` *and* `FrameClock` |
| "a `GameTick` system must pick an `App` **and** a clock" | **gone** — there is one of each |
| `Sim.entity_interp` (a `World` nested in a `World`) | **deleted** — 15 fields → 14 |
| `PlayerSnapshot`'s vitals collapsing into `Vitals`/`Xp`/`ServerEntityId` | **done, but (c) was not the only thing in the way** — see [below](#the-vitals-collapse-and-the-second-blocker-c-hid) |

---

## How it works

### The handle travels down, never up

```
Sim::build          →  App with every plugin  →  World  →  Arc<RwLock<World>>   ── Sim.ecs
Sim::connect        →  NetClient::connect(host, port, protocol, Some((ecs, local)))
                    →  ClientBuilder::ecs(world, session)
                    →  SharedState::adopting(world, session)
```

`Sim` owns the `World` and hands it down. The reverse — `SharedState` minting it and the driver
adopting it — is **not an alternative**, for a reason that is easy to miss: `Sim.local` (the local
player's `Entity`) is held across `Sim::end_session` by the voluntary-teardown path, whose acceptance
test is a genuine second connect. A `World` whose identity changed at each connect would invalidate
that `Entity` every time.

`SharedState::adopting` inserts **nothing**. `SharedState::default` (the no-driver path — bots,
`tests/read_model.rs`) still seeds `WorldTime` and a `ChunkWorld`; doing either in `adopting` would
zero a live clock and would steal the chunk-store adoption decision from `Sim::adopt_live_world`,
which owns it — including the `collide_against_live_world` negative control that depends on naming an
explicitly *empty* store.

### One entity, because one `World`

`spawn_local_player` and `spawn_session` both spawn an entity carrying the `LocalPlayer` marker. In
separate `World`s that was fine; in one `World` it would give every `With<LocalPlayer>` system two
players and the HUD would read whichever the query happened to yield first. So `Sim::build` calls
all three on one entity:

```rust
let local = spawn_local_player(&mut ecs, player);   // physics, intent, hotbar, fly, wire edges
insert_hud_components(&mut ecs, local);             // phase, vitals, xp, overlays, chat, respawns
insert_session_components(&mut ecs, local);         // scoreboard, tab list, boss bars, menus
```

`the_one_world_holds_exactly_one_local_player` asserts the count, and
`a_separately_spawned_session_entity_makes_two_local_players` is its control — it spawns the session
entity separately (exactly what `SharedState::default` does) and observes 2.

The direct consequence: `Sim::{sidebar, player_rows, boss_bars, player_menu, open_menu}` read
components off `Sim.local` instead of round-tripping through `NetClient` → `ClientHandle` →
the client's `World`. Still one fold (`lodestone_ecs::session`'s `NetIngest` systems), still one
copy; only the reader changed.

### The plugin list, and the one trap in it

`Sim::build`'s `App` now installs `IngestPlugin` and `SessionPlugin` — the *net thread's* folds —
because there is one `World` and this is it. **Exactly once**: `SessionPlugin` guards the shared
`drain_ingest_queue` behind `is_plugin_added::<IngestQueuePlugin>()`. `add_systems` does not
deduplicate, and two copies of that system silently blank every batch the first one filled (Stage 3
shipped that as a total ingest blackout whose unit tests stayed green because they installed one
plugin).

`EntityInterpPlugin` joins them, and `EntityInterpolator` stops being a `World` owner in production.

---

## The clock: one accumulator, and the policy decision

### What was wrong

Measured by Stage 5 and recorded in [`sim-dissolution.md`](./sim-dissolution.md): two independent
20 Hz accumulators, on two different catch-up policies.

| | the player's | the interpolator's |
|---|---|---|
| where | `FrameClock::accumulator` (`f64`), `Sim::step` | `TickAccum` (`f32`), `EntityInterpolator::update_with_view` |
| fed | `dt.clamp(0.0, 0.25)` — **five** ticks | the pacer's already-clamped `dt`, **unclamped** — ten |

`FramePacer::begin_frame` clamps `dt` to `MAX_CATCHUP_SECS = 0.5 s`; `Sim::step` then clamped
*again* to `0.25 s`. So a maximal stall advanced item physics and the walk cycle **five ticks
further** than player physics — per stall, cumulatively, with no mechanism to reconcile, because the
excess real time was discarded rather than carried. `end_session` reset one accumulator (by
replacing the whole interpolator) and not the other, re-phasing them arbitrarily on every
quit-to-title.

The `f32`-vs-`f64` term is real and irrelevant: `0.05f32` against `1.0/20.0` is ~1.5e-8 relative,
about one tick per 39 days.

### The policy, and why ten won

**`lodestone_ecs::MAX_CATCH_UP_TICKS = 10`.** Unifying forced a choice and this is it:

- it is vanilla's own `MAX_TICKS_PER_UPDATE` (`Minecraft.java:262`, applied at `:1176`), which is the
  only *external* oracle either candidate has;
- it is what [`frame-pacing.md`](./frame-pacing.md) documents and what `FramePacer` already clamped
  to, so the driver's two clamps now agree instead of one silently shadowing the other;
- the tighter `0.25 s` had no derivation anywhere in the tree. It predates the frame pacer, and its
  only written justification was the pacing test's own observation that it bound first ("measured
  **5**, not 10") — a record of the discrepancy, not a reason for it. That assertion said out loud
  "if this changed, reconcile the two caps".

Cost of loosening: the worst-case catch-up burst is ten physics ticks in one frame instead of five.
That is vanilla's own worst case and the frame pacer was already sized for it.

**What was updated to match.** `app.rs`'s `a_long_stall_is_clamped_not_replayed` now asserts `10`
and says why; `MAX_TICKS_PER_UPDATE`, `TICK_SECS` and `MAX_CATCHUP_SECS` in `app.rs` are now
**aliases** of the `lodestone-ecs` constants rather than local re-derivations, and the test asserts
the two are the same constant — a local copy that agreed today is precisely how the five-vs-ten
divergence started. `sim.rs`'s `TICK_DT` is deleted.

### The mechanism

`FrameClock` owns the loop, so it is written once rather than per driver:

```rust
clock.begin_frame(dt);          // secs += dt (unclamped); accumulator += dt.clamp(0, 0.5); frames += 1
while clock.take_tick() { … }   // accumulator -= TICK_PERIOD; ticks += 1
clock.end_frame();              // interp_alpha = accumulator / TICK_PERIOD
clock.reset_accumulator();      // teardown: residual only, never `secs`
```

`secs` takes the **unclamped** `dt` and the accumulator the clamped one. That asymmetry is
load-bearing: `secs` answers "how long ago did this chat line arrive" and must track wall time across
a stall, while the accumulator answers "how many ticks do we owe" and must not replay a minute of
them. `reset_accumulator` therefore never touches `secs` — a chat line stamped before a
quit-to-title still ages correctly after it.

`interp_alpha` is now the single sub-tick residual: the camera's between-tick ease and
`extract_entity_draws`'s walk-cycle partial tick both read it, where they used to read two
accumulators' residuals.

### The frame, in order

```
apply_mouse
clock.begin_frame(dt)
Egress ← phase == Connected && is_live()
FrameDelta ← dt ;  run_schedule(Update)      ← FrameSet::{Input, Interpolate, Camera, Terrain}
while clock.take_tick():
    PlayerCollision ← tick_collision()
    ItemCollision   ← item_collision()
    run_schedule(GameTick)                   ← TickSet::{Input, Physics, Predict, Animate, Send}
    drain_action_queue()                     ← everything TickSet::Send queued, in order
    tick_particles()
clock.end_frame()
poll_net()
fold_entity_snapshots()
run_schedule(Extract)                        ← ExtractSet::{Terrain, Entities, Hud}
refresh_stats()
```

Two ordering facts are load-bearing:

1. **`Update` runs *before* the tick loop.** `FrameSet::Interpolate`'s `advance_interp_clocks` must
   run first, because `tick_item_physics` and `tick_walk_animation` measure off the *drawn* pose and
   would otherwise measure last frame's. That ordering used to be internal to
   `EntityInterpolator::update_with_view`; it is now the frame's.
2. **`fold_entity_snapshots` still runs after the tick loop and after ingest**, which is the order
   the ~25 interpolation tests are written against.

**One behaviour change rides on (1):** `FrameSet::Terrain`'s `heal_dirty_columns` now runs *before*
`poll_net`, so a column that arrives this frame has its neighbours healed on the next one. It is a
coalescing drain feeding an async worker pool on a per-frame budget, and the total
arrival→upload latency already spans several frames, so one more is inside the noise — but it is a
change, not a no-op, and it is the thing to look at first if chunk seams regress.

### `ItemCollision` — the resource that must **not** be shared

`tick_item_physics` used to read `PlayerCollision` in the interpolator's `World`. Merging the
`World`s would have silently merged two genuinely different decisions:

| case | the player's `PlayerCollision` | items' `ItemCollision` |
|---|---|---|
| live, the player's column not streamed | `Pending` — hold the player rather than drop them | fall back to the chunk store; an item elsewhere still has a floor |
| `collide_against_live_world = false` | an explicitly **empty** store, so the player falls through | the real chunk store, so the negative control does not also disable item physics |

`sim-dissolution.md` flagged exactly this for `tick_particles` and predicted the fix would be "a
second per-tick collision resource with its own documented decision". It is.

---

## Lock discipline

Three rules, on `lodestone_ecs::EcsHandle`. None of them is style.

> ### Rule 1 broke the client, and the record was not wrong
>
> `accb993` **hard-froze on the first tick of the first block dig**: 72 fps for thirty seconds, a
> status line showing a live pick target, and then nothing — no panic, no error, no log line, a
> window that had to be force-quit. A silent stop is a hang, not a crash.
>
> `crate::interact::drive_mining` resolved the held item with
> `net.get().map(ClientHandle::player_menu)`. `SharedState::player_menu` takes `self.ecs.read()`;
> `drive_mining` is a `TickSet::Send` system, so it runs inside `run_schedule(GameTick)`, which runs
> inside `hold_write`. Write guard held, read guard requested, same `parking_lot::RwLock`, same
> thread. `crates/lodestone-shell/tests/mining_deadlock.rs` reproduces it hermetically and is the
> gate; its control observes `player_menu()` wedging under the guard while
> `ClientHandle::block_at()` returns normally in the same guard.
>
> **The interesting part is that this section already said so.** `player_menu` is named in the
> rule-1 set below, and has been since the vitals collapse. What went wrong was the *exception*: the
> paragraph establishing that the four chunk-backed reads are legal from a system was read as
> clearing `ClientHandle` in general. It clears four methods, and the sentence naming
> `drive_mining`'s `block_at` as legal sat two lines above the list containing the method that
> deadlocked it. A correct, carefully-hedged note is not a mechanism.
>
> So there is a mechanism now, and one deleted call site:
>
> - **`hold_read`/`hold_write` panic on reentrancy** instead of hanging. They keep a thread-local
>   ledger of guards (handle address + `#[track_caller]` location) and abort with a message naming
>   *both* sites. The three always-fatal combinations (write⊃read, write⊃write, read⊃write) abort in
>   every build; read⊃read is conditional — it deadlocks only when a writer is queued — so it aborts
>   under `debug_assertions` and is left alone in release, rather than trading an intermittent hang
>   for a certain crash. `handle.rs`'s tests carry both halves: the fatal cases panic, and two
>   negative controls (two *different* worlds nesting, and ten *sequential* guards) prove it does not
>   fire on the normal path.
> - **`NetHandle::get` is private.** The `ClientHandle` no longer leaves the resource; what
>   `interact.rs` exposes is `block_at` and nothing else, so a future system cannot name a
>   `World`-backed accessor to begin with.
> - **`drive_mining` reads the `SessionMenus` component** off the local player instead. This is what
>   §4.1(c) was *for*: there is one `World`, the ingest fold writes `SessionMenus` into it, and the
>   round trip through `ClientHandle` was returning a clone of bytes already in the `World` the
>   system was holding — at the cost of a 46-slot `Menu` clone per tick and, as it turned out, the
>   whole client.
>
> Still only-by-review: **the ledger only sees guards taken through `hold_read`/`hold_write`.**
> `lodestone_client::state`'s ~12 accessors call `self.ecs.read()` directly, so a *new* reentrant
> call through one of them still hangs rather than panicking. Routing them through `hold_read` closes
> that and also fills the gap `LockHolds` documents (the net thread's own holds are unmeasured); it
> is the obvious next change and is deliberately not in this one, which is release-blocking.

1. **Never hold a guard across a call that might take the same lock.** `parking_lot::RwLock` is
   neither reentrant nor upgradable: `write()` → `read()` on one thread deadlocks instantly, and
   `read()` → `read()` deadlocks whenever a writer is already queued (that is what `read_recursive`
   is for, and nothing here relies on it). Concretely, the driver must not hold a guard while calling
   into `NetClient`/`ClientHandle` — **every** read on those locks this same `World` now.
   `Sim::fold_entities` resolves `net.entity_snapshots()` to an owned `Vec` *before* taking its
   guard, for exactly this reason.

   **"Every read" is too strong, and the exception is load-bearing.** The *chunk*-backed reads —
   `block_at`, `sections_and_light_at`, `world_dimensions`, `loaded_chunks` — take only
   `SharedState`'s chunk-store lock and never touch the ECS `World`. That is what makes two shipped
   call sites legal rather than deadlocks: `crate::interact::drive_mining` reads
   `NetHandle::block_at` from inside `run_schedule(GameTick)` (so, under the `World` write guard),
   and `crate::net::entity_light_at` does the same lookup for entity lighting. Both are `World →
   chunks`, i.e. rule 3, not rule 1. The rule-1 set is the *ECS*-backed reads: `entities`,
   `entity_snapshots`, `world_time`, `tab_list`, `scoreboard`, `boss_bars`, `player_menu`,
   `open_menu`, `menu_click`, and — **new with the vitals collapse** — `player`,
   `local_player_attributes` and everything derived from `player` (`health`, `food`, `is_alive`,
   `game_mode`, `experience_*`).

   **`ClientHandle::player` changed sides, and that is the one hazard the collapse introduced.** It
   used to touch only `SharedState`'s scalar lock, so it was safe to call from anywhere; it is built
   from components now. The shell's one production call site is `Sim::refresh_mesh_policy` (via
   `mesher::sky_default_for_dimension`), which runs at the top of `poll_net` with no guard held, and
   `mesher::snapshot_section_live`'s only caller is `tests/live_world_mesh.rs`, likewise unguarded —
   both audited at the time of writing. `ClientHandle::position` and `rotation` are deliberately
   **not** in the rule-1 set: they read the local echo directly and take no ECS lock, because they
   are the reads a moving bot makes most often and there is nothing in the component set they need.
2. **Never hold a guard across an `.await`.** `lodestone_client`'s driver already promised this for
   the scalar read-model (`state.rs`'s module docs); it now matters for this lock too, because a task
   parked with the `World` write-locked would stall the frame.
3. **`World` before chunks, never the reverse.** The driver takes this lock and *then* (inside a
   system, or inside `tick_particles`) the `ChunkWorld` lock. The net thread takes the chunk lock for
   `handle_packet` and **releases it before folding events** (`driver.rs` scopes that guard
   deliberately), so it only ever takes this one afterwards. Both orders are `World → chunks`.
   Reversing either side is an ABBA deadlock and nothing in the type system stops it —
   `tick_particles` is written inside-out (store handle cloned, `World` guard taken, chunk guard
   taken inside) specifically to obey this, because the obvious spelling is the wrong order.

**Rule 1 is enforced structurally for writes and only by review for reads.** `Sim::write` and
`write_local` take `&mut self`, so the borrow checker forbids the closure reaching another accessor
and so the same lock a second time. `Sim::read` takes `&self` and gets no such protection: a `read`
closure that called another `&self` accessor would take a second read guard and deadlock behind any
queued writer. Audited at the time of writing — no `read`/`write` closure in `sim.rs` calls any method
on `self` — but that is a review property, not a compiler one.

`Sim` enforces (1) structurally rather than by convention: there is no accessor that returns a guard
or a `&`-into-the-`World`. Reads return owned values (`player() -> PlayerState`, which is `Copy`;
`session_phase() -> SessionPhase`; `particle_instances() -> Vec<_>`) and writes take a closure
(`player_mut(|p| …)`, `input_mut(|i| …)`, `terrain_mut(|t| …)`). The private helpers are
`Sim::read(f)` / `Sim::write(f)` / `Sim::write_local(f)`; `write` takes `&mut self` **so the borrow
checker forbids reaching another accessor from inside the closure**, and `write_local` exists only
because that `&mut` then makes `self.local` unreadable inside it.

Rule 1 is not theoretical. Writing a test helper as
`handle.write().query_filtered::<…>().iter(&handle.write())` — two guards in one expression — hung
the test binary. It is now one named guard, with a comment saying why.

### Can ingest stall the frame? Can the frame stall ingest?

**Both, and the honest answer is worse than §4.1(a) implies.**

§4.1(a) says the net thread "must keep draining regardless of frame rate… a slow frame delays
*application*, never *receipt*". That is true of the socket→`ClientEvent` channel but **not** of the
`World` lock, because `SharedState::apply` runs **inline in the driver task**: `Driver::run` reads a
packet, executes directives, and `emit` calls `read_model.apply(&event)` *before*
`events.send(event).await`. `apply` takes `ecs.write()`. On a `current_thread` runtime, blocking
there blocks the whole driver task — so it stops reading the socket too. Before (c) that lock was
uncontended (only the net thread ever wrote the client's `World`); now the driver contends for it.

What bounds the damage is that **no guard spans the frame**. `Sim::step` never takes one long guard.
Ingest's own hold is one `run_schedule(NetIngest)` for one event. So the worst case a packet can wait
is *one guard hold*, not *one frame*.

Three deliberate choices keep the longest hold small:

- `Sim::particle_instances` returns an owned `Vec` rather than a mapped read guard. The guard version
  would keep the `World` read-locked for the whole GPU upload — the same failure inverted, with the
  frame stalling ingest. A `memcpy` of a few thousand POD instances is the cheaper side of that
  trade.
- `drain_action_queue` takes the queue out under a guard and releases it before `net.send_action`.
- **`Sim::with_particles_unlocked` moves the whole emitter out of the `World`**, runs the
  `O(live particles)` pass with **no guard held at all**, and moves it back. Both per-frame particle
  passes go through it. See below.

### The bound is now measured, not counted off the code

§4.1(c) shipped the paragraph above as an argument from reading the source — which is exactly the
**duration** species of vacuous test `CLAUDE.md` names: a claim about how long something takes with
nothing that looks at how long it takes. `lodestone_ecs::LockHolds` is the counter that looks.

Every guard `Sim` takes goes through `lodestone_ecs::hold_read` / `hold_write`, which fold the hold
duration into a `LockHolds` resource (interior `AtomicU64`s, because a **read** guard yields `&World`
and the reads are most of the guards). `CorePlugin` inserts it; a `World` without it is silently
unmeasured rather than a panic. `Sim::lock_holds()` / `Sim::reset_lock_holds()` read and zero it.
The clock starts *after* acquisition, so this measures **how long we held it** — not how long we
waited for it, which is the other side of the same coin and would not be attributable.

Measured on an M-series laptop, hermetic (`cargo test -p lodestone-shell --lib -- --nocapture`):

| | wall | guarded | holds | longest |
|---|---|---|---|---|
| `extract_particles`, 4 000 live particles | 371 µs | 57 µs | 4 | **41 µs** |
| the same call in its **pre-fix shape** (extract inside the guard) | 329 µs | 330 µs | 2 | **328 µs** |
| one `Sim::step(0.1)`, demo world | 134 µs | 119 µs | 35 | **40 µs** |

Read those three rows carefully, because two of them are the point:

- **Row 2 is the negative control, and it fires.** `the_pre_fix_shape_of_extract_particles_fails_the_hold_bound`
  reproduces the old spelling and asserts it *fails* the bound row 1 passes. Without it, row 1 is a
  ratio nobody has shown can come out the other way.
- **Rows 1 and 3 agree, which is the real finding.** The longest hold in `extract_particles` is now
  the same ~40 µs as the longest hold in a whole frame — i.e. the particle path is no longer the
  outlier, and **the longest remaining hold in the process is one `run_schedule` call**, as the
  original structural argument assumed but could not show. 35 holds per frame, also measured, is
  what "many short guards" means.
- The assertion in row 1 is a **ratio against the call's own wall time** (guarded < 25%), not an
  absolute nanosecond ceiling: an absolute bound is a statement about one machine. Row 3's ceiling
  *is* absolute (25 ms — "no guard spans a 40 fps frame") because a whole `step` legitimately is
  mostly its two `run_schedule` holds, so a ratio there would assert nothing; its control is
  `lodestone-ecs`'s `the_hold_meter_reports_a_deliberately_long_hold`, which sleeps 30 ms under a
  guard and observes the counter report it.

Two honest limits on the above. First, **41 µs is more than two resource moves should cost** and is
not fully explained; it is within scheduling noise at this scale and it is not the number the
assertion rests on (the ratio is), but do not read it as a floor. Second, the hermetic light closure
is the offline arm (`self.net == None`), so the *live* per-particle chunk lookups are absent from
row 1 — which is the whole point rather than a gap: those lookups now happen with **no `World` guard
held**, so their cost cannot enter the hold however large particle volume gets. To see the live
magnitude anyway, run `scripts/live-oracles/creative.sh`, break a large volume of blocks (or stand in
rain) and read `Sim::lock_holds()`.

**What the meter does not cover:** the net thread's own holds. `SharedState::apply` takes
`ecs.write()` directly; routing that one call through `lodestone_ecs::hold_write` would put ingest's
`run_schedule(NetIngest)` hold on the same counter. That is a one-line change in `lodestone-client`
and was out of scope here.

For scale: keep-alive timeouts are seconds and the measured guard holds are tens of microseconds, so
this is a latency question, not a disconnect risk.

### `tick_particles` no longer navigates rule 3 — it retires it

`tick_particles` used to be written inside-out on purpose (chunk store handle cloned, `World` guard
taken, chunk guard taken *inside*) because the obvious spelling — take the chunk read guard, then
reach for the emitter — is `chunks → World`, the one order that can ABBA against the net thread. It
now goes through `with_particles_unlocked`, so the chunk guard is taken inside a closure that holds
**no** `World` guard, and the two are never held together at all. Do not read the trailing
`insert_resource` write as "chunks then `World`": the chunk guard is a temporary inside the closure
and is gone before it.

The one thing `with_particles_unlocked` costs is an **absence window** — between its two guards the
`World` has no `ParticleSim`. Nothing can observe it: `&mut self` makes it exclusive on the driver
thread, the closure runs no schedule, the only other reader (`crate::interact::drive_mining`, a
`TickSet::Send` system) is on that same thread, and the net thread's `NetIngest` systems live in
`lodestone-ecs` and cannot name a `lodestone-shell` resource at all. A panic inside the closure would
leave it missing, which is what the `expect` message says.

---

## How to change it

- **Adding a system:** one `App`, one clock. `CorePlugin` gives you the four schedules and their
  public sets; add your plugin to `Sim::build`'s `add_plugins` tuple. Guard any *shared* registration
  with `is_plugin_added` — `add_systems` does not deduplicate.
- **Reading the `World` from outside the driver:** `Sim::ecs()` hands out the `EcsHandle`. Take a
  short guard. Re-read the three rules above first; the failure mode is a hang, not an error.
- **Adding a `Sim` accessor:** return an owned value, or take a closure. Do **not** return a guard or
  a reference into the `World`, however tempting — that is how rule 1 gets violated by a caller who
  never read this file.
- **Adding a per-frame resource the schedules read:** insert it in `Sim::step` before the schedule
  that reads it, in the same `write` block as the `run_schedule` where possible, so it is one guard
  rather than two.
- **Changing the catch-up policy:** `lodestone_ecs::MAX_CATCH_UP_TICKS`. `app.rs`'s constants alias
  it and its pacing test asserts they are the same constant, so there is one place.
- **A session teardown must reset three things explicitly** — the accumulator
  (`FrameClock::reset_accumulator`), the entity tracks (`entities::reset_entity_tracks`) and the
  component sets (`reset_local_player` + `insert_hud_components`). The first two used to be side
  effects of dropping a `World`; they are now visible calls in `Sim::end_session`, which is the point,
  and both have gates (`end_session_resets_the_one_accumulator_and_not_the_monotonic_clock`,
  `end_session_clears_the_entity_tracks`).

### `EntityInterpolator` survives as a harness

It still owns a `World`, and that is deliberate: the `#[ignore]`d live GPU gates
(`tests/live_entity_render.rs`, `tests/live_dropped_item.rs`) drive interpolation against a bare
`NetClient` with no `Sim` anywhere, and so do this module's ~25 unit tests. It runs the *same*
systems in the same order off the *same* `FrameClock` type — a second *instance* of one mechanism,
not a second mechanism. `TickAccum` is deleted; there is no second accumulator **type** left in the
tree.

Production reaches the same systems through free functions on `&mut World`:
`fold_entity_snapshots`, `extracted_entity_draws`, `tracked_entity_count`, `reset_entity_tracks`,
`set_item_stack_in`.

---

## The vitals collapse, and the second blocker (c) hid

`lodestone_client::state::PlayerSnapshot`'s vitals used to duplicate `Vitals` / `Xp` /
`ServerEntityId`. Stage 3 bounded that residue by "the §4.1 `World` unification", and **that was not
the whole blocker** — (c) shipped and the duplication was still there. The second one:
`SharedState::apply` routes each `ClientEvent` to **exactly one** of two folds:

```rust
if TimeChanged            { …ECS resource… }
else if ingest::handles_event(e) || session::handles_event(e) { …ECS systems… }
else                      { echo.apply(e) }           // now: TeleportPlayer only
```

The events that carry the vitals did not carry *only* the vitals:

| event | ECS-side, before | `Inner`-side, before |
|---|---|---|
| `Login` | `ServerEntityId` | `game_mode`, `dimension`, `alive` |
| `HealthChanged` | `Vitals{health, food, saturation}` | `alive = health > 0.0` |
| `Respawned` | `RespawnCount` | `dimension`, `game_mode`, `alive` |
| `Death` | (`Dead`, gated — below) | `alive = false` |
| `ExperienceChanged` | `Xp` | — |

So claiming any of the first four for a `NetIngest` system would have stopped `Inner::apply` seeing
it: `dimension` freezes, which is the too-bright-Nether bug reached by traversal, fixed in `fc6b6c6`
and one careless routing change away from returning. Only `ExperienceChanged` was free, and
collapsing one field of six buys nothing.

### The decision: move the rest of the fold, do not weaken the routing

Two options, and the second was taken:

1. **Make the routing non-exclusive** — run the ECS schedule *and* `Inner::apply` for events both
   claim. Cheap, and it leaves one event with two folds writing two copies of `dimension`. That is
   the double-fold defect `docs/session-components.md` exists to delete, re-created deliberately;
   the routing switch's documented invariant would have had to be rewritten to permit it.
2. **Move `game_mode`, `dimension` and `alive` into components too**, so no event carries a field
   the scalar side still owns, and the routing stays exclusive with no wording changed.

(2) costs three new components — `lodestone_ecs::session::{ServerGameMode, ServerDimension,
ServerAlive}` — one new system (`apply_local_player_state`), and it makes `PlayerSnapshot` a
**derived** value, which is the same sanctioned intermediate Stage 1 established for `EntityView`:
components authoritative, struct derived, never the reverse. `PlayerSnapshot`'s public shape is
unchanged, so `ClientHandle`'s accessors, `mesher.rs`'s `player().dimension` and all 27 of
`tests/read_model.rs` were untouched by it.

What is left behind the client's scalar lock is `LocalEcho { position, rotation, on_ground }` and
`TeleportPlayer` as its only fold arm. Stage 2 was right that `PlayerSnapshot` as a whole stays split
from the `LocalPlayer` *prediction* components — but the split is narrower than it looked: only the
echo is genuinely a different fact. The vitals were a plain duplicate.

Two consequences worth knowing before touching this:

- **`ClientHandle::player` is ECS-backed now**, so it joins rule 1 above. `position`/`rotation` are
  not: they read the echo and take no ECS lock.
- **`NetUpdate::Health` and `NetUpdate::Experience` are deleted**, along with their `forward` arms and
  `Sim::poll_net` arms, for the same reason Stage 3 deleted `TabListEvent`/`ScoreboardEvent`: the net
  thread folds those events into the components the HUD already reads, so a shell-side arm would be a
  *second writer of one component*. `NetUpdate::{Death, Respawned}` stayed, because they drive the
  driver's own `Dead` marker and `RespawnCount`, which are not folds of the server's view.

### `alive` and `Dead` did not merge, and that is the point

**`ServerAlive` and `crate::player::Dead` are not the same fact.** `ServerAlive` is `false` on
`Death` **and** on any `HealthChanged` with `health <= 0`, `true` on `Login`/`Respawned`/a positive
`HealthChanged`. The `Dead` marker is inserted only on `NetUpdate::Death` and removed only on
`Respawned`, *and it is gated on `Sim.recover_from_death`* — the live death gate's negative control
flips that to reproduce "stranded on the death screen forever". Merging them deletes that control.
`zero_health_kills_and_positive_health_revives_without_a_death_packet` pins both directions of the
`ServerAlive` rule and asserts that neither inserts `Dead`.

### A stale note found on the way, and fixed

`Sim::end_session`'s doc said the tab list, scoreboard, boss bars and menus "need no clearing at all
… they are components in the *client's* `World`, so dropping `net` drops the only route to them".
**True when written, false since (c).** There is one `World`, the readers are
`Sim::sidebar`/`player_rows`/`boss_bars` off `Sim.local`, and dropping `net` drops no route to
anything — the previous server's sidebar and tab list survived a quit-to-title. `end_session` now
calls `insert_session_components` beside `insert_hud_components`, and
`end_session_tears_down_and_a_fresh_connect_afterward_starts_clean` asserts the sidebar clears.

---

## What `Sim` is down to

**14 fields** (28 before Stage 5, 15 after it). `entity_interp` is gone. `ecs` is still a field but
is no longer *blocked*: it is the shared handle now, so the shape of deleting `Sim` is "`WindowApp`
holds the `EcsHandle`, every `Sim` method becomes a system or a free function over `&mut World`" —
mechanical, with nothing structural in the way. Of the rest, `net` remains genuinely blocked
(`NetClient` holds an `mpsc::Receiver`, which is `Send` but **`!Sync`**, so it can never be a
`Resource`; the `NetHandle`/`SharedHandle` seam is how systems reach the client instead), and the
other twelve are unfinished mechanical work — see
[`sim-dissolution.md`](./sim-dissolution.md#not-blocked-just-not-done).

## Configuration

Nothing new. `--features live` is still the only version selector. `lodestone_ecs::TICK_PERIOD`,
`MAX_CATCH_UP_TICKS` and `MAX_CATCH_UP_SECS` are the tick-policy constants; `app.rs` aliases them.

## Dependencies

- `lodestone-ecs` re-exports `parking_lot`, because `EcsHandle` is a `parking_lot::RwLock` and a
  consumer that wants to *name* a guard type must spell it with the same `parking_lot` this crate
  locked with. Matching the version by hand in each manifest is how you get two `RwLock`s that look
  identical and are not the same lock.
- `lodestone-client` gains `ClientBuilder::ecs(world, session)` and
  `SharedState::adopting(world, session)`. No new external dependency — it already depended on
  `lodestone-ecs`.
- `lodestone-shell`: `NetClient::connect` gains a fourth parameter; `Sim::connect` is the wrapper
  every non-loopback caller should use.

## Two pre-existing breaks found by running the second health check

Neither is from this change, and both were invisible to `cargo check --workspace --all-targets`
because they live in `#[cfg(feature = "live")]` test code in `sim.rs`:

- `use std::time::Instant` was missing from
  `live_bare_hand_stone_timing_survives_the_real_hardness_seam` (introduced by `15d08e2`).
- `the_registry_seam_feeds_the_same_numbers_the_unit_tests_assume` still read the `Sim.version_data`
  *field*, which Stage 5 deleted in favour of the `VersionData` resource.

Both are fixed here. The lesson is the one `CLAUDE.md` already states — `--all-targets` alone misses
non-default features — but it is worth recording that it caught two more, in a crate whose default
test suite was entirely green.
