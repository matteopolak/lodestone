# The server tick clock: MSPT/TPS accounting and overrun handling

This covers a real 20Hz tick loop, independent of client traffic, plus
MSPT/TPS accounting and tick-overrun handling.

## What it is

`crates/lodestone-server/src/tick.rs`: [`TickClock`]/[`TickStats`] plus
`run_tick_loop`, the single background task that advances world state (the
mob simulation and every registered block entity) at vanilla's 20Hz, whether
or not a client is connected. It replaces two separate background loops
(`mobs::run_mob_tick_loop`, `block_entities::run_block_entity_tick_loop`) that
used to be spawned side-by-side, and adds MSPT/TPS accounting:
ticks-run, most-recent and rolling-average tick duration (MSPT), a derived
TPS figure, and an overrun counter for when the loop falls behind schedule.

## Before this: six timers, not three

An earlier analysis of this codebase undercounted the timers already in
`crates/lodestone-server/`. The real list, found by grepping
`Instant|Duration|interval|sleep|elapsed` across the crate:

| timer | file (before this work) | cadence | scope |
|---|---|---|---|
| `MOB_TICK_INTERVAL` | `mobs/mod.rs` (`run_mob_tick_loop`) | 50ms | world (background) |
| `BLOCK_ENTITY_TICK_INTERVAL` | `block_entities.rs` (`run_block_entity_tick_loop`) | 50ms | world (background) |
| `KEEP_ALIVE_INTERVAL` | `server.rs` | 15,000ms | per-connection |
| `TIME_SYNC_INTERVAL` | `server.rs` | 1,000ms | per-connection |
| `VITALS_TICK_INTERVAL` | `server.rs` | 50ms | per-connection |
| `CONTAINER_SYNC_INTERVAL` | `server.rs` | 50ms | per-connection |

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

## Recommendation reversed: `lodestone-ecs` is now linked into the server

**This reverses `server-tick-loop.md`'s linking recommendation, while preserving both
architectural findings it encoded: the server owns its clock, and the server never runs inside the
client's schedule.**

The owner has decided to adopt `bevy_ecs` in `lodestone-server`, so that server-side plugins get the
same five-clause intent doctrine [`docs/plugin-api.md`](./plugin-api.md) already gives client-side
plugins — matching Bukkit/Spigot's own precedent of implementing core server functionality through
the plugin surface itself, rather than beside it. A read-only architecture review designed the
migration; the decision record and the new subsystem this unlocks are
[`docs/server-ecs.md`](./server-ecs.md). What follows is why the three legs that used to block this
no longer do — each re-verified against source for this pass, not restated from the earlier
analysis:

1. **File-ownership collision — task-scoped and expired. Void.** The original blocker was that
   `lodestone-ecs` sat in a different agent's file-ownership split for one concurrent-editing task.
   That scoping has expired; it was never an architectural argument on its own and carries no weight
   now.
2. **"Bridging means a second threading model to reconcile with tokio" — the first horn is
   empirically void in this workspace; the second horn is fully preserved.** The *second* horn —
   running the server's tick from *inside* the client's own schedule, which would break the
   integrated-server premise that singleplayer (and open-to-LAN, with no render loop attached at
   all) must keep advancing on its own — is still the wrong design and stays rejected. The tokio
   loop and its clock remain the sole driver; nothing about that changes.

   The *first* horn does not hold, checked directly rather than assumed: `bevy_app`/`bevy_ecs` are
   pinned **without** `multi_threaded` on every target, workspace-wide (root `Cargo.toml`:
   `bevy_app = { version = "0.19", default-features = false, features = ["std"] }` /
   `bevy_ecs = { version = "0.19", default-features = false, features = ["std"] }`).
   `crates/lodestone-ecs/Cargo.toml`'s own comment on the same two lines: "`multi_threaded` does not
   even compile on wasm32 with no threads... left off on every target so native and wasm run the
   same executor and the same system order." With no multi-threaded executor compiled in,
   `World::run_schedule(...)` is a plain synchronous call on the calling thread — there is no second
   runtime and no thread pool for tokio to reconcile with.

   `crates/lodestone-ecs/src/runner.rs`'s `Runner::Headless` variant already demonstrates exactly
   this, and has since before this decision. `Runner::Headless { tick_hz, max_catch_up_ticks }`
   and its `run_headless` method are a hand-rolled `while`
   loop built on `std::time::Instant`/`std::thread::sleep` — no tokio, no executor — that calls
   `app.world_mut().run_schedule(GameTick)` directly. The server's tick loop takes
   the identical shape: `tokio::time::sleep_until` drives the wake-up (unchanged from today), and the
   schedule run itself is one more synchronous call on the tick task, exactly like `mobs.tick()` and
   `block_entities.tick_all()` are today.
3. **`run_tick_loop` is a plain spawned function — unchanged, and it remains the driver.**
   `crate::tick::run_tick_loop` is still `pub(crate) async fn`
   (`crates/lodestone-server/src/tick.rs`), still its own `loop { ... sleep_until ... }`, and
   still reached from exactly one call site — `spawn_tick_task`
   (`crates/lodestone-server/src/integrated.rs`), a plain wrapper around `tokio::spawn` +
   a shutdown `tokio::select!`, invoked from `open_in_memory_with_mobs_using`. Adopting `bevy_ecs` changes what
   runs *inside* one iteration of that loop — `world.run_schedule(GameTick)` in place of calling
   `MobSim::tick`/`BlockEntityRegistry::tick_all` directly — not who calls the loop or how often.

See [`docs/server-ecs.md`](./server-ecs.md) for the subsystem this reversal unlocks: two `World`s,
never one, and the plugin-adjudication window a scheduled apply makes possible that an inline
connection-task mutation never could.

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

**Vanilla catches up — just not indefinitely**, and the one thing CLAUDE.md's
brief specifically asked to *verify rather than assume* ("does not try to
catch up indefinitely") is the "not indefinitely" half, not a claim that no
catch-up happens at all. `nextTickTimeNanos += thisTickNanos` runs
*unconditionally*, every iteration, whether or not the server is behind
(`MinecraftServer.runServer`), and `waitUntilNextTick`/`haveTime`
does not park at all once real time has already reached
`nextTickTimeNanos`. So while only mildly behind, iterations run back-to-back
at full speed with zero artificial delay between them — that *is* the
catch-up, and `run_tick_loop` gets it for free from `tokio::time::sleep_until`
resolving immediately for an already-past deadline; no separate code path
was needed for it.

The forgiveness branch is not that mechanism — it is what happens once
catching up the normal way would take too long. When the loop is more than 2
seconds behind schedule, it gives up on that remaining backlog specifically:
it logs a rate-limited warning and jumps `next_tick_at` forward by however
many tick periods it was behind — the world tick body still runs **exactly
once** per loop iteration, both before and after that adjustment, and that
one backlog is forgiven rather than replayed (smaller backlogs are still
replayed by the back-to-back iterations above). [`TickClock::record_tick`] is
only ever called once per *real* iteration, so `tick_count` reflects ticks
actually run, never `wall_clock_elapsed / 50ms`.

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
  `MinecraftServer`'s field of the same name) for the rolling average.
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
  each still has its own direct unit test (`mobs/mod.rs`,
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
`run_tick_loop` is spawned from two constructors — see
[One loop per world, not per connection](#one-loop-per-world-not-per-connection)
below for the LAN one, which is newer than the rest of this
section. The singleplayer chain is
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
[`IntegratedServer::tick_stats`] — `Some` for `open_in_memory_with_mobs` and
(since the LAN world-tick addition) `bind`, `None` for the remaining in-memory constructors. No shell
UI reads it yet (out of
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

### One loop per world, not per connection

For a long time `run_tick_loop` had exactly **one** caller,
`open_in_memory_with_mobs`. `IntegratedServer::bind` — the open-to-LAN path —
never spawned it, and conceded as much in a comment. So over LAN block entities
held state but never advanced, scheduled and fluid ticks never drained, random
ticks never fired, mobs never ticked, and `game_tick` never incremented, while
join, keep-alives, chunk streaming and entity movement all looked perfectly
healthy. `bind` now spawns it.

**The count is the hard part, not the spawn.** "Spawn a tick loop" has an
obvious wrong implementation — spawn it inside the accept arm, i.e. once per
connection — which yields a world advancing at N× speed with N players. That
reads as a physics bug (mobs too fast, furnaces too quick) for a long time
before anyone suspects the loop count, and it is exactly the straddle
[`server-ecs.md`](./server-ecs.md) forbids: a *world* concern living on a
*connection*.

So the loop is spawned once, **outside** the accept loop, over the same
`Arc<S>` source and the same `BlockEntityHandle`/`MobHandle` every connection
shares. Note it is per-`IntegratedServer`, **not** global: `bind` and
`open_in_memory_with_mobs` build different worlds over different sources, so
"both entry points share one loop" would be wrong.

**`tick_stats()` cannot detect a duplicated loop, and that is worth
internalising.** A per-connection loop would carry its own `TickClock`, so the
server handle's stats would report a healthy 20 TPS while the world ran at 40.
`tests/lan_world_tick.rs` therefore uses a second, independent instrument: a
counting `ChunkSource` whose probe chunk sits inside the tick area and outside
every connection's view, so its `column()` count is attributable purely to tick
loops — all of them, whichever clock they report to. The assertion is a **ratio**
between a zero-connection and a two-connection measurement taken moments apart
in the same process, which makes it immune to build profile and machine speed.
Observed, `--release`:

| arm | probe visits/s |
|---|---|
| 0 connections | 19.30 |
| 2 connections (correct fix) | 19.97 — ratio 1.04 |
| 2 connections (per-connection loop control) | 59.98 — ratio 3.11, gate fails |

The 3.11 is 1 world loop + 2 connection loops, exactly as predicted.

**The achieved rate is an environment property, not one specific to this
addition — and the first version of this note blamed the wrong variable.**
Because `run_tick_loop`
re-generates every column in its area from the source on *every* tick (the
documented chunk-lifecycle gap), it is sensitive to whatever else the machine is doing:

| run | probe visits/s | nominal |
|---|---|---|
| shared checkout, unoptimised, ~4 concurrent agents building | 2.66 | 20 |
| shared checkout, `--release` | 19.29 | 20 |
| isolated worktree, **unoptimised**, machine idle | **19.29** | 20 |

This doc briefly said the gap was debug-versus-release, on the strength of the
first two rows. The third row — the *same* unoptimised build on an idle machine,
matching release exactly — shows the dominant variable is **concurrent machine
load**. Two observations at two different moments, and the difference attributed
to the wrong cause; the isolated-worktree re-run is what caught it, which is a
second reason to do that run beyond confirming the commit is green.

The gate therefore asserts no absolute rate. Its comparison is a ratio between
two windows measured moments apart under whatever conditions hold, which is
immune to both variables.

**LAN's tick area is a fixed constant, `integrated::LAN_TICK_RADIUS = 2`**
(a 5×5 chunk square around the origin), because this crate still has no
loaded-chunk registry to derive one from. That is conservative rather than
arbitrary: singleplayer already passes `view_radius.clamp(1, 3)`, i.e. up to
7×7, with the *real* generator. `docs/plans/chunk-lifecycle.md` is what
replaces it with a ticket-driven set. `bind`'s public signature deliberately did
not grow a parameter for it.

#### The feed fan-out LAN needs and singleplayer does not

`BlockTickFeed` and `ExplosionFeed` are append-and-**drain-all**: the first
consumer takes everything and a second sees nothing. Singleplayer has exactly
one connection per feed instance, so this never mattered. For LAN both obvious
options are defects:

- hand every connection the *same* feed → every player but one silently misses
  random-tick block updates;
- hand the tick loop a feed nobody drains → unbounded growth while the server
  idles with zero clients.

So each LAN connection gets its **own** feed pair, and a relay drains the tick
loop's hub feed and re-publishes into all live subscribers. The relay is a third
arm of `bind`'s existing accept `select!` rather than a fourth task, which is
what keeps it free: no extra `Task` field, it cannot outlive `shutdown`, and the
subscriber list is touched by that one task alone so it needs no lock.
`LiveMobSource` needs none of this — it is a replace-latest-snapshot cache every
connection diffs independently, so it is already multi-consumer safe and is
shared directly.

Gotcha found while writing it: `LanSubscriber`'s `Default` is **hand-written**,
because `Arc<AtomicBool>::default()` is `false` — a derived `Default` would mark
every new subscriber dead and the relay would prune it before publishing
anything. A fan-out that silently delivers nothing, and no compile error.

The better long-term shape is still the per-connection cursor over a shared
append-only log that `BlockTickFeed`'s own doc comment recommends: it needs no
copy per subscriber. Build that if the subscriber count grows past a handful.

## Configuration

- `integrated::LAN_TICK_RADIUS` (2) — the LAN tick area's Chebyshev radius; see
  above for why it is a constant and not a `bind` parameter.
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
