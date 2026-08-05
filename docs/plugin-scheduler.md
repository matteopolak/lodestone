# The plugin task scheduler

## What it is

`TaskScheduler` (`crates/lodestone-ecs/src/scheduler.rs`) is the delayed-and-repeating task API for
plugins — Bukkit's `BukkitScheduler.runTaskLater` / `runTaskTimer`, which is the most-used non-event
surface in the Java plugin ecosystem. A plugin schedules a closure to run on a future `GameTick`,
once or on a period, instead of hand-rolling a countdown component plus a per-tick system that checks
it. Issue #113.

## How it works

Three pieces, all in one module:

- **`TaskScheduler`**, a `Resource`. `schedule_once(delay_ticks, f)` and
  `schedule_repeating(delay_ticks, period_ticks, f)` each return an opaque `TaskId`; `cancel(id)`
  removes it. Scheduling is a plain `&mut self` push with no `World` access, so it composes with any
  system in any schedule.
- **`run_due_tasks`**, an **exclusive** (`&mut World`) system anchored in `TickSet::Input`.
- **`SchedulerPlugin`**, which installs both. Opt-in, like `GameEventBusPlugin`.

```rust,ignore
use lodestone_ecs::{SchedulerPlugin, TaskScheduler};
use lodestone_ecs::ecs::system::ResMut;
use lodestone_ecs::ecs::world::World;

fn arm_a_countdown(mut sched: ResMut<TaskScheduler>) {
    // 60 ticks (3 s) from now, once.
    sched.schedule_once(60, |world: &mut World| {
        let _ = world;
    });
    // Every 5 ticks, starting 5 ticks from now.
    let id = sched.schedule_repeating(5, 5, |_world: &mut World| {});
    // Store `id` in your own resource to `cancel` it later.
    let _ = id;
}
```

### Tick semantics, exactly

Off-by-one *is* the difficulty of a scheduler, so the contract is a firing **schedule**, not a vague
"later":

| call | fires on `GameTick` |
|---|---|
| `schedule_once(0, f)` | 1 (next tick) |
| `schedule_once(1, f)` | 1 (next tick) |
| `schedule_once(n, f)` | `n` |
| `schedule_repeating(delay, period, f)` | `delay`, `delay + period`, `delay + 2·period`, … |

`period_ticks == 0` is clamped to 1 (every tick). Over `N` ticks a repeating task fires
`1 + (N - delay) / period` times for `N >= delay`.

`schedule_once(0, ..)` meaning "next tick" rather than "immediately" matches Bukkit's
`runTaskLater(plugin, task, 0)`. A task never runs inside the tick that scheduled it — see the
reentrancy section.

### Why the closure takes `&mut World`

Issue #113 named the signature as the open design question and flagged that `FnMut(&mut World)`
"re-opens the reentrancy question if the closure is invoked while a guard is already held elsewhere."
It does not, and the distinction is worth keeping:

`run_due_tasks` is exclusive. By the time bevy calls it, the driver has already taken the `EcsHandle`
write guard and passed the resulting `&mut World` down through `World::run_schedule(GameTick)`. The
`&mut World` a task receives **is that same borrow, one stack frame deeper**. Nothing re-locks
anything, so a task cannot deadlock against the guard the driver holds — it never acquires a guard.

The hazard is real but is a *different* hazard: a closure that captured an `EcsHandle` clone and
called `hold_write` on it. That is `docs/plugin-async-tasks.md`'s territory (issue #114), where a
runtime guard catches it.

`Commands` was the alternative and is strictly weaker: a task that wants to read state to decide what
to do — "is the player still holding the item?", the common case — cannot, because `Commands` only
queues. `&mut World` subsumes it (`world.commands()`).

### Reentrancy: due tasks are moved out, the list is not

The obvious implementation is `World::resource_scope::<TaskScheduler, _>`, which lifts the resource
out of the `World` for the duration of the callback. That breaks a plugin doing something entirely
reasonable — **a task that schedules another task** — because
`world.resource_mut::<TaskScheduler>()` inside the closure then panics with "requested resource does
not exist".

So `run_due_tasks` moves out only the tasks that are *due*, leaving `TaskScheduler` present and
mutable for the whole drain. A task closure may schedule, cancel, or inspect. Anything it schedules
is considered from the *next* tick, never re-entered this one, so a task that schedules itself with
`delay 0` cannot spin forever inside one tick.

Self-cancellation works too: `cancel` records the id, and the re-arm step at the end of each task
consults that list, so a repeating task cancelling its own id is not put back.

### Where it runs, and the deliberate gap

`TickSet::Input` — the earliest anchor in `GameTick`, and empty until now. That is Bukkit's own
placement: tasks run at the *start* of a tick and observe the previous tick's finished state, which
is why a task can usefully write `MovementIntent` and have `TickSet::Intent` and `TickSet::Physics`
act on it the same tick.

There is **no end-of-tick pool**, and that is named rather than silent: it needs a second ordering
anchor, and adding an anchor variant is a plugin-ABI change that goes through `docs/plugin-api.md`'s
ordering-anchor changelog (issue #170). A task needing end-of-tick semantics schedules with `delay 1`
and reads the previous tick's result — the same information, one tick later.

## How to change it, and the gotchas

- **Task closures are `Send + Sync`.** `TaskScheduler` is a bevy `Resource`, which is
  `Send + Sync + 'static`, so a task may not capture an `Rc`. Use `Arc` (and `parking_lot::Mutex`,
  re-exported from `lodestone-ecs`, if it needs interior mutability). This is the one constraint that
  bites plugin authors, and it surfaces as a trait error on the closure rather than at the call.
- **`run_due_tasks` uses `swap_remove`, which scrambles the list.** Removing a due task moves a
  *not*-due task into the vacated slot, so the loop index must **not** advance on removal, and the
  due list is re-sorted by `TaskId` before running so "order is schedule order" holds.
  `tests::removing_a_due_task_does_not_make_a_later_task_skip_a_tick` is the gate that catches
  breaking either half.
- **Do not gate a repeating-task test on "it fired at least once."** Every wrong implementation also
  fires at least once — `CLAUDE.md`'s *magnitude* species of vacuous test. Assert the exact tick
  indices: a delay off by one produces the same *number* of firings, so a count cannot see a phase
  error. `tests::a_repeating_task_fires_on_exactly_the_predicted_ticks` computes both the correct and
  the off-by-one schedule from outside constants and requires the measurement to land on one.
- **Check for accumulation.** A once-task must leave no entry behind, or a plugin scheduling one task
  per tick grows the list without bound. `pending()` exists so that is observable;
  `tests::a_finished_once_task_leaves_no_entry_behind` and its repeating-task negative twin are the
  pair.
- **Register through the plugin, not by hand, in tests.** A test that calls `run_due_tasks(&mut world)`
  directly proves nothing about whether the runner drives it — the island `CLAUDE.md` rule 1 names.
  `tests::with_no_scheduler_plugin_a_hand_inserted_task_never_fires` and its twin
  `with_the_scheduler_plugin_the_same_hand_inserted_task_does_fire` differ *only* in the
  registration, which is what makes the first one evidence.

## Configuration

None. `SchedulerPlugin` is opt-in: `app.add_plugins(SchedulerPlugin)`. It adds `CorePlugin` itself if
absent, so it is the only plugin a scheduler-using crate needs to name. Nothing in the shipped client
adds it yet — same status as `GameEventBusPlugin` and `crates/plugins/lodestone-autopilot`, and the
same reason (`docs/plugin-api.md` §Configuration: there is no plugin-loading mechanism yet).

## Dependencies

`bevy_app`, `bevy_ecs`, and `crate::{CorePlugin, GameTick, TickSet}`. No new external dependency —
the scheduler is a `Vec` and a countdown, not an executor.

## See also

- [`docs/plugin-async-tasks.md`](./plugin-async-tasks.md) — issue #114's off-tick half, and the
  reentrancy guard that catches the hazard this doc says the scheduler does not create.
- [`docs/plugin-api.md`](./plugin-api.md) — the surface this is part of, and the ordering-anchor
  policy that stops an end-of-tick anchor being added casually.
- [`docs/bevy-migration.md`](./bevy-migration.md) §4.2 — where `TickSet` and `GameTick` come from.
