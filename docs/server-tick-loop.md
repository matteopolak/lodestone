# The server tick clock: MSPT/TPS accounting and overrun handling

Issues [#284](https://github.com/matteopolak/lodestone/issues/284) (a real
20Hz tick loop, independent of client traffic) and
[#285](https://github.com/matteopolak/lodestone/issues/285) (MSPT/TPS
accounting and tick-overrun handling).

## What it is

`crates/lodestone-server/src/tick.rs`: [`TickClock`]/[`TickStats`] plus
`run_tick_loop`, the single background task that advances world state (the
mob simulation and every registered block entity) at vanilla's 20Hz, whether
or not a client is connected. It replaces two separate background loops
(`mobs::run_mob_tick_loop`, `block_entities::run_block_entity_tick_loop`) that
used to be spawned side-by-side, and adds the accounting #285 asked for:
ticks-run, most-recent and rolling-average tick duration (MSPT), a derived
TPS figure, and an overrun counter for when the loop falls behind schedule.

## Before this: six timers, not three

An earlier analysis of this codebase undercounted the timers already in
`crates/lodestone-server/`. The real list, found by grepping
`Instant|Duration|interval|sleep|elapsed` across the crate:

| timer | file:line (pre-#284) | cadence | scope |
|---|---|---|---|
| `MOB_TICK_INTERVAL` | `mobs.rs:1783` (`run_mob_tick_loop`) | 50ms | world (background) |
| `BLOCK_ENTITY_TICK_INTERVAL` | `block_entities.rs:407` (`run_block_entity_tick_loop`) | 50ms | world (background) |
| `KEEP_ALIVE_INTERVAL` | `server.rs:40` | 15,000ms | per-connection |
| `TIME_SYNC_INTERVAL` | `server.rs:54` | 1,000ms | per-connection |
| `VITALS_TICK_INTERVAL` | `server.rs:107` | 50ms | per-connection |
| `CONTAINER_SYNC_INTERVAL` | `server.rs:553` | 50ms | per-connection |

Only the first two were *world* clocks running independently of any one
client — those are the two this work unifies. The other four are
legitimately per-connection: keep-alive is a health check for one socket;
time-sync, vitals, and container-sync all read or write **one player's own**
state (the tracked position, the tracked open container). Folding a
per-connection cadence into a world clock would not close an island, it
would just misname one — a lone connection's vitals tick has no business
surviving that connection's disconnect. They are unchanged by this work, and
this doc's job is partly to record *why*, so nobody re-derives "unify the six
timers" as "delete the other four" from a shorter brief later.

## Recommendation re-verified: do not link `lodestone-ecs` into the server

A prior analysis recorded that the server should own its own clock rather
than borrow the client's ECS schedule. Re-checked directly for this work,
not assumed:

- `crates/lodestone-server/Cargo.toml` has no `lodestone-ecs` dependency, and
  adding one would create a second problem beyond scope: `lodestone-ecs` is
  explicitly off-limits in this task's file-ownership split (owned by the
  interaction-intent seam agent), so depending on it here would collide with
  concurrent work regardless of the architectural question.
- More fundamentally, `lodestone-ecs`'s schedule is *client*-side and
  `bevy_ecs`-shaped (systems, resources, a `World`) — the server's own state
  (`MobSim`, `BlockEntityRegistry`) is plain Rust behind `Arc<Mutex<_>>`
  handles shared with `tokio::spawn`ed connection tasks, not an ECS `World`.
  Bridging the two would mean either running an ECS schedule inside the
  server (a new, heavyweight dependency and a second threading model to
  reconcile with tokio) or running the server's tick from *inside* the
  client's own schedule (which breaks singleplayer's own premise: the
  integrated server must keep advancing even if the render loop stalls, and
  must be the thing open-to-LAN serves with no render loop attached at all —
  see `lib.rs`'s own module doc for why `IntegratedServer` speaks the same
  loop for both).
- The finding holds: `run_tick_loop` is a plain `tokio::spawn`ed async
  function with no ECS involvement, exactly like the two loops it replaces.

## How it works

```text
IntegratedServer::open_in_memory_with_mobs(..)
  ├─ spawns the connection task (unchanged): serve_connection, diffing
  │  LiveMobSource::snapshots() / BlockEntityHandle against what the
  │  connection last sent
  └─ spawns tick::run_tick_loop(mob_handle, live_mobs, block_entities, clock)
       next_tick_at = now
       last_overload_warning_at = None
       loop:
         now = Instant::now()
         (next_tick_at, last_overload_warning_at, overrun) =
             resolve_overload(now, next_tick_at, last_overload_warning_at)
         if let Some(event) = overrun { warn!(...); clock.record_overrun() }
         next_tick_at += TICK_PERIOD   // 50ms
         sleep_until(next_tick_at).await
         mobs.tick(); mob_out.publish(mobs.snapshots()); block_entities.tick_all()
         clock.record_tick(elapsed)
```

`resolve_overload` is a **pure function** (`Instant, Instant, Option<Instant>
-> (Instant, Option<Instant>, Option<OverloadEvent>)`) deliberately extracted
out of the loop — see "Why the overrun branch is tested this way" below.

### Overrun handling, ported from vanilla exactly

Vanilla's own tick loop (`.cache/mc/26.2/src/net/minecraft/server/MinecraftServer.java`)
does **not** try to catch up indefinitely. The relevant lines:

```java
// MinecraftServer.java:197, 199 (constants)
private static final long OVERLOADED_THRESHOLD_NANOS = 20L * TimeUtil.NANOSECONDS_PER_SECOND / 20L; // 1s
private static final long OVERLOADED_WARNING_INTERVAL_NANOS = 10L * TimeUtil.NANOSECONDS_PER_SECOND; // 10s

// MinecraftServer.java:734-743 (runServer, the tick loop)
long behindTimeNanos = Util.getNanos() - this.nextTickTimeNanos;
if (behindTimeNanos > OVERLOADED_THRESHOLD_NANOS + 20L * thisTickNanos
   && this.nextTickTimeNanos - this.lastOverloadWarningNanos >= OVERLOADED_WARNING_INTERVAL_NANOS + 100L * thisTickNanos) {
   long ticks = behindTimeNanos / thisTickNanos;
   LOGGER.warn("Can't keep up! Is the server overloaded? Running {}ms or {} ticks behind", ...);
   this.nextTickTimeNanos += ticks * thisTickNanos;   // <- forgives the backlog, does not replay it
   this.lastOverloadWarningNanos = this.nextTickTimeNanos;
}
```

With `thisTickNanos` fixed at 50ms (this codebase has no `TickRateManager`/
sprinting to vary it), the two derived thresholds are:

- **Overload threshold: 2 seconds.** `1s + 20 * 50ms`.
- **Warning re-fire interval: 15 seconds.** `10s + 100 * 50ms`.

`resolve_overload` computes both from `TICK_PERIOD` rather than hardcoding
2000ms/15000ms, so a future change to the tick period (there is currently no
reason to make one) keeps the same vanilla-derived relationship.

The key behavior, and the one CLAUDE.md's brief specifically asked to
*verify rather than assume*: when the loop is more than 2 seconds behind
schedule, it does **not** run the missed ticks to catch up. It logs a
rate-limited warning and jumps `next_tick_at` forward by however many tick
periods it was behind — the world tick body still runs **exactly once** per
loop iteration, both before and after that adjustment. The backlog is
forgiven, never replayed. [`TickClock::record_tick`] is only ever called once
per *real* iteration, so `tick_count` reflects ticks actually run, never
`wall_clock_elapsed / 50ms`.

### One subtlety found while re-deriving vanilla's own behavior

Vanilla's `lastOverloadWarningNanos` is a bare Java `long`, default-initialized
to `0` — not to `nextTickTimeNanos`. So on a real server's very first overload
ever, `nextTickTimeNanos - 0` is enormous, and the warning-interval check is
trivially satisfied: **the first overload always warns.** An earlier version
of this code seeded the Rust equivalent (`last_overload_warning_at`) to the
loop's own start instant, making that gap exactly zero on the very first
check — which would have silently swallowed the very first overload forever.
Fixed by making it `Option<Instant>`, `None` until the first warning ever
fires (`tick.rs`, `resolve_overload`'s own doc comment;
`resolve_overload_fires_on_the_very_first_overload_with_no_prior_warning`
pins it).

## MSPT/TPS accounting

[`TickClock`] holds:

- `tick_count` — real (never-skipped) ticks run.
- `last_mspt_micros` — the most recently completed tick's own duration.
- a 100-sample ring buffer (matching vanilla's own `tickTimesNanos` array,
  `MinecraftServer.java:248`) for the rolling average.
- `overrun_count` — how many times the loop has forgiven a backlog.

[`TickStats`] (returned by [`TickClock::stats`], and by
[`IntegratedServer::tick_stats`]) derives `tps` as
`1000.0 / mspt_avg_ms.max(50.0)` — vanilla never reports faster than 20 TPS
even when the average tick is comfortably under budget, so the tick *period*
is a floor a full tick cannot beat, matching the server's own debug-HUD TPS
derivation.

## How to change it, and the gotchas

- **`run_mob_tick_loop`/`run_block_entity_tick_loop` still exist.** They are
  no longer what `IntegratedServer::open_in_memory_with_mobs` spawns, but
  each still has its own direct unit test (`mobs.rs`,
  `block_entities.rs`), so they are marked `#[allow(dead_code)]` rather than
  deleted — deleting them would also delete real regression coverage on
  `MobSim::tick`/`LiveMobSource::publish` and `BlockEntityRegistry::tick_all`
  composing correctly in isolation. If you are tempted to resurrect either as
  a *third* production loop: don't — see "Islands" below.
- **`resolve_overload` is pure on purpose.** `tokio::time::advance` (the
  standard way to test timers under `#[tokio::test(start_paused = true)]`)
  cannot exercise the overrun branch at all: it fires every timer between the
  current and target instant *in order*, re-polling the task at each one —
  so a 3-second `advance` produces 60 healthy, on-schedule ticks, never one
  forgiven backlog. Measured directly while building this: an async test
  built exactly that way asserted `tick_count == 1, overrun_count == 1` and
  got `tick_count == 60, overrun_count == 0`. There is no `tokio::time` API
  that jumps the virtual clock without firing intervening timers, so testing
  this branch **requires** calling the pure function with hand-built
  `Instant`s, not spawning the loop. See `tick.rs`'s test module for the
  full boundary/rate-limit suite this produced.
- **If you add a seventh timer, ask which side of the six-timer table it
  belongs on first.** A per-connection cadence (something that reads or
  writes one player's own tracked state) belongs in `server.rs`'s
  `serve_play`; a world-simulation cadence belongs in this loop.

### Islands, and what actually consumes this

Per CLAUDE.md's "nothing is done until something on screen changes":
`run_tick_loop` is spawned exactly once, from
`IntegratedServer::open_in_memory_with_mobs`
(`crates/lodestone-server/src/integrated.rs`), which is the constructor
`crates/lodestone-shell/src/net.rs`'s `run()` calls for `Origin::Integrated`
(singleplayer) — unchanged call site, unchanged signature, so no shell edit
was needed for this work. The consumer chain, end to end:

```text
net.rs::run() (shell, unchanged)
  -> IntegratedServer::open_in_memory_with_mobs (integrated.rs)
       -> spawns tick::run_tick_loop
            -> MobSim::tick / BlockEntityRegistry::tick_all (world state advances)
            -> LiveMobSource::publish (mob snapshots visible to every connection)
       -> spawns the connection task: serve_connection
            -> EntityStreamer::sync diffs LiveMobSource against what was last
               sent, every inbound packet, and container_sync_tick diffs
               BlockEntityHandle the same way on its own per-connection timer
            -> real client-bound packets (add_entity/move_entity, container
               slot updates) reach the wire
```

`TickStats` itself is consumed through
[`IntegratedServer::tick_stats`] — `None` for every constructor except
`open_in_memory_with_mobs`, `Some` there. No shell UI reads it yet (out of
this task's scope: the shell files this task touches are `net.rs`, and no
debug HUD/command surface exists in this crate to plug it into today); the
accessor exists so a future debug overlay or `/tps`-style admin command has
something real to call rather than needing to invent the plumbing too.
Verified as reachable, not just present, by
`crates/lodestone-render/tests/…` is not applicable here — the equivalent
gate for this crate is `tick.rs`'s own test module plus
`integrated_memory.rs`'s existing (unmodified, still-passing) coverage of
`open_in_memory_with_mobs` continuing to prove the mob/block-entity wiring
end to end.

## Configuration

- `tick::MILLIS_PER_TICK` / `tick::TICK_PERIOD` — the 20Hz period. Not the
  same constant as `server.rs`'s own private `MILLIS_PER_TICK`; see
  `tick.rs`'s own module doc for why the two are allowed to independently
  agree rather than share one definition.
- `tick::HISTORY_LEN` (100) — the MSPT rolling-average window, matching
  vanilla's `tickTimesNanos` size.
- `overload_threshold()` / `overload_warning_interval()` — derived from
  `TICK_PERIOD`, not separately configurable; see the vanilla citations above
  if these ever need to change.

## Dependencies

- `tokio::time` (`Instant`, `sleep_until`) — native only, exactly like the
  two loops this replaces; unavailable on `wasm32`.
- `tracing` — newly added to `lodestone-server`'s `Cargo.toml` for the
  overload warning log; this crate had no logging dependency before.
- `crate::mobs::{MobHandle, LiveMobSource, MobSim}`,
  `crate::block_entities::{BlockEntityHandle, BlockEntityRegistry}` — the
  world state this loop advances; unchanged by this work.

[`TickClock`]: ../crates/lodestone-server/src/tick.rs
[`TickStats`]: ../crates/lodestone-server/src/tick.rs
[`TickClock::stats`]: ../crates/lodestone-server/src/tick.rs
[`TickClock::record_tick`]: ../crates/lodestone-server/src/tick.rs
[`IntegratedServer::tick_stats`]: ../crates/lodestone-server/src/integrated.rs
