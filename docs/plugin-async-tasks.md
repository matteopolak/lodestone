# Async plugin tasks and the World hand-back

## What it is

`AsyncTaskPool` (`crates/lodestone-ecs/src/async_task.rs`) lets a plugin run blocking work off the
tick thread — a database query, an HTTP call, a pathfinding search — and get the result back into the
`World` safely. Results arrive either by polling a `PendingTask<T>` from a system or through a
hand-back closure the pool runs with `&mut World` at a tick boundary. This is Bukkit's
`runTaskAsynchronously` plus `runTask`. Issue #114.

## How it works

### The issue as written cannot be done soundly

Issue #114 asks for "`spawn_async(future) -> impl Future<Output = T>` with a documented 'here is how
you get `T` back into a system safely'". Read literally — a future a plugin awaits wherever it likes,
which then touches the `World` — **that is unsound here**, for three independent reasons:

- `docs/world-unification.md`'s Lock Discipline **rule 2** forbids awaiting while holding a `World`
  guard. A plugin-authored future that both awaits *and* reaches the `World` is a construction whose
  only defence is review.
- `parking_lot::RwLock` is neither reentrant nor upgradable. `handle.rs`'s own `check` table shows
  three of the four held/requested combinations **cannot make progress at all**. A *second thread*
  taking a guard is legitimate — the net thread does it — but a thread that is inside plugin work and
  takes a guard is how you get a hang with no panic, no error and no log line, this repo's worst
  failure mode (`accb993`).
- The server-ECS decision was that the server's `bevy_ecs::World` is **tick-thread-owned with no lock
  at all**. There is not even a guard to take on that side, so an API premised on off-thread `World`
  access could not be honoured there in principle.

So the implemented shape is the sound variant, and the soundness is a property of the **types**:

| half | signature | can it reach the `World`? |
|---|---|---|
| off-tick work | `FnOnce() -> T + Send` | **no argument to reach it with** |
| hand-back | `FnOnce(T, &mut World) + Send` | yes — and it runs on the tick thread |

The off-tick closure takes **no parameters**: no `&World`, no `&mut World`, no `EcsHandle`. The
ordinary way to violate rule 2 is not discouraged, it is unrepresentable. The hand-back does get
`&mut World`, and runs inside `drain_completed_tasks`, an exclusive system, where the borrow it
receives is the driver's own guard passed one frame deeper — the same argument as
`docs/plugin-scheduler.md`'s.

### The API

```rust,ignore
use lodestone_ecs::{AsyncTaskPool, AsyncTaskPoolPlugin, PendingTask};
use lodestone_ecs::ecs::world::World;

// Recommended: hand-back. Nothing has to remember to poll.
fn kick_off(pool: &AsyncTaskPool) {
    pool.spawn_with_handback(
        || expensive_lookup(),                       // off-tick; no World access possible
        |result: u32, world: &mut World| {           // tick thread, at a schedule point
            let _ = (result, world);
        },
    );
}

// Alternative: poll a handle, e.g. as a component on the entity it belongs to.
fn kick_off_polled(pool: &AsyncTaskPool) -> PendingTask<u32> {
    pool.spawn(|| expensive_lookup())
}

fn expensive_lookup() -> u32 { 0 }
```

`PendingTask<T>` derives `Component`, so the common shape is `world.spawn(task)` plus a
`Query<(Entity, &PendingTask<T>)>` system that calls `try_take()`. `try_take` moves the value out and
returns `None` afterwards, while `is_finished()` stays `true` — two states, deliberately
distinguishable, so a poller cannot double-handle.

### The one hole the types cannot close, and the runtime guard that does

A plugin can still **capture** an `EcsHandle` clone in the off-tick closure and call `hold_write` on
it from the worker thread. No signature prevents that: `EcsHandle` is `Arc<RwLock<World>>`, which is
`Send` because it has to be.

Issue #114 asks for "a compile-time lint if feasible". It is **not** feasible — Rust has no negative
trait bound for "this closure captures no `EcsHandle`" — so the next best thing is implemented: a
**loud panic instead of a hang**. Every pool worker marks its thread with a `Cell<bool>` thread-local
for the thread's whole lifetime, and `handle.rs`'s `Ledger::enter` — the ledger `hold_read`/`hold_write`
already run for rule 1 — refuses on a marked thread with a message naming rule 2, the call site, and
the sound alternative.

The check is **always on**, not `debug_assertions`-only, because a check that silently vanishes in
release is `CLAUDE.md`'s *precondition* species of vacuous test. It costs one thread-local read per
guard, which is negligible against acquiring the `RwLock` it precedes.

**What the guard is blind to** — named rather than left to be discovered:

- `EcsHandle` is a type **alias** for `Arc<RwLock<World>>`, so `handle.read()` and `handle.write()`
  are `parking_lot`'s own inherent methods and **cannot be intercepted**. A worker calling them
  directly still hangs. This is the same gap issue #20 ("route the ~12 direct `ecs.read()` calls
  through `hold_read`") exists to close; closing it makes this guard total. Today it covers the
  sanctioned path and nothing more.
- Threads a plugin spawns itself with `std::thread::spawn` are not marked. The guard speaks for *this
  pool's* workers, which is all the pool can honestly speak for.

### Scheduling and ordering

`drain_completed_tasks` is exclusive, anchored in `TickSet::Input`, ordered **before**
`scheduler::run_due_tasks`. Both are exclusive systems in the same set, so without an explicit edge
their relative order is whatever the topological sort picks. `AsyncTaskPoolPlugin` therefore adds
`SchedulerPlugin` if absent and declares the edge, making "a hand-back's effect is visible to a
scheduled task the same tick" a fact rather than a coincidence. The coupling is one-directional — the
scheduler knows nothing about the pool.

*Which* tick a given hand-back lands on is inherently nondeterministic; it depends on when the worker
finished, and nothing here pretends otherwise. What is deterministic is that it runs on the tick
thread, at a schedule point, never concurrently with a tick.

### wasm32

No threads (`docs/bevy-migration.md` §3.1). On that target `spawn`/`spawn_with_handback` run the
closure **inline on the caller** and queue the result exactly as a worker would, so the API and the
completion path are identical — the "off-tick" property is the one thing that is not.
`AsyncTaskPool::runs_work_inline()` reports this, so a plugin that must not block can branch on it
rather than assume. The worker marker is set for the inline run too, so the rule-2 panic is not
target-specific.

## How to change it, and the gotchas

- **Never add an off-tick API that hands out `World`, `&World`, or an `EcsHandle`.** The whole
  soundness argument is that the off-tick closure has no parameter to reach the world with. An
  overload "just for the pathfinder" would delete the argument.
- **A worker that panics must not kill the worker thread.** `worker_loop` wraps each job in
  `catch_unwind`, because a dead worker silently stops draining the queue and every later job hangs
  forever — a far worse symptom than the original panic. Issue #168 is the general version;
  `tests::a_panicking_job_does_not_kill_the_worker` bounds it here.
- **`drain_completed_tasks` takes the completions list out before running anything.** A hand-back that
  spawns more work would otherwise deadlock on the pool's own mutex — the same reentrancy shape
  `scheduler::run_due_tasks` solves, and just as easy to reintroduce.
- **`PendingTask`'s slot is a separate `Arc` from the pool.** A handle must outlive the pool, or a
  plugin swapping resources invalidates handles a system is still holding.
- **Measure with a count, never a duration.** `PoolStats` is four counters. `CLAUDE.md` is explicit
  that a timing taken while other work is live gets attributed to the wrong cause, and every question
  worth asking here ("did anything run off-tick?", "did every job hand back?") is a counting question.
  The tests use a 10 s deadline purely as a *bound* against hanging — nothing asserts on elapsed time.
- **Default is two worker threads.** Plugin off-tick work is latency-bound, not throughput-bound, and
  `CLAUDE.md`'s machine notes are emphatic about unbounded thread and memory growth here.
  `AsyncTaskPool::with_threads(n)` for more.
- **The rule-2 gate needs its control.** `a_worker_that_takes_a_world_guard_panics_naming_rule_two` is
  worthless without `the_identical_guard_on_the_tick_thread_succeeds`: a guard that panicked
  *unconditionally* would pass the first test while breaking the entire client. The pair is what makes
  it evidence.

## Configuration

None. `app.add_plugins(AsyncTaskPoolPlugin)` — it pulls in `SchedulerPlugin` and `CorePlugin` itself.
Nothing in the shipped client adds it yet, same status and reason as `SchedulerPlugin` and
`GameEventBusPlugin` (`docs/plugin-api.md` §Configuration).

## Dependencies

`bevy_app`, `bevy_ecs`, `parking_lot` (already a dependency, for the `Mutex` and `Condvar`), and
`std::thread` on native. **No new external dependency** — deliberately no `tokio`, no `rayon`, no
`bevy_tasks`: `crates/lodestone-ecs/Cargo.toml` has none of them, and the pool is a `Vec`, a condvar
and two threads.

## See also

- [`docs/plugin-scheduler.md`](./plugin-scheduler.md) — issue #113's on-tick half; the drain here is
  ordered against its system.
- [`docs/world-unification.md`](./world-unification.md) — Lock Discipline rules 1 and 2, which this
  module enforces the second half of.
- [`docs/plugin-api.md`](./plugin-api.md) — the surface this belongs to.
