# Server plugin scheduler

## What it is

`lodestone_server::ecs::ServerTaskScheduler` is the native server-plugin task
surface for delayed callbacks and off-tick work. It keeps all mutable server
world access on the primary tick owner while allowing bounded background work
to return a value through a queued hand-back.

## How it works

`ServerCorePlugin` installs one scheduler resource and runs
`run_server_tasks` in `GameTick`'s `TickSet::Drain`. A synchronous task scheduled
with `schedule_once(delay, callback)` runs after `max(delay, 1)` completed game
ticks. `schedule_repeating(delay, period, callback)` then repeats every
`max(period, 1)` ticks. Equal deadlines retain registration order, callbacks
may schedule or cancel other tasks, and work scheduled from a callback starts
on a later pass.

`spawn_with_handback(work, hand_back)` runs the parameterless `work` closure on
a native worker and queues its result. The `hand_back` closure receives the
result and `&mut World` only on the primary tick owner, before synchronous due
callbacks. Browser builds execute `work` inline but retain the same next-pass
hand-back boundary. The scheduler reserves one bounded slot for both running
work and queued results, so admission returns `ServerAsyncTaskError::Full`
instead of blocking or growing an unbounded queue.

`cancel_async` prevents an unfinished result from reaching the world; it does
not interrupt a worker already running. `shutdown_async_tasks` rejects new
work, cancels outstanding jobs, and discards queued completions. Worker panics
are contained and release their reservation without invoking a hand-back.
Dropping the scheduler performs the same shutdown transition.

The production integrated-server constructors move the configured `World` into
their primary tick task after `ServerBoot`. The production gate in
`crate::ecs::gate` exercises that real boundary and asserts the exact delayed
and repeating trace, rather than testing only a hand-built scheduler world.

## How to change it

Keep the scheduler's queue and clock private to the tick owner. New world-facing
effects belong in a hand-back or synchronous callback, never in the worker
closure. Preserve the single reservation bound when changing completion
storage: a completed result must continue to hold capacity until the tick owner
handles or discards it. Add a focused lifecycle test for every new terminal
state, and assert exact tick indices for delay or repetition changes.

When changing production wiring, update the constructor path that calls
`ServerApp::bootstrap_with` and the primary tick loop together. A scheduler unit
test cannot prove that the extracted `World` is the one the live server drives.

## Configuration

`ServerCorePlugin` uses `DEFAULT_ASYNC_HAND_BACK_CAPACITY` (64). An embedding
that needs a different bound can replace the resource during
`ServerApp::bootstrap_with` with
`ServerTaskScheduler::with_async_hand_back_capacity(capacity)`. Capacity must
be greater than zero. There is no persistence for task handles or pending
callbacks; they end with the owning world.

## Dependencies

The scheduler depends on `bevy_ecs::World` and the server's `GameTick`/
`TickSet` schedules. Native workers use `std::thread` and a bounded
`std::sync::mpsc` channel; browser builds use no worker thread. Production
world ownership and lifecycle are supplied by `IntegratedServer` and
`ServerApp`.
