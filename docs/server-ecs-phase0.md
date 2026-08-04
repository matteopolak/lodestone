# Server ECS, Phase 0: the `App` exists and is provably live

## What it is

Phase 0 of [`docs/plans/server-ecs-migration.md`](./plans/server-ecs-migration.md) — the dependency
edge and `ServerCorePlugin` that give `lodestone-server` its own `bevy_ecs::World`, plus the gate that
proves the `World` is *live* rather than an inert scaffold. It migrates no state and does not make the
tick loop drive the `World`; that is Phase 1. [`docs/server-ecs.md`](./server-ecs.md) is the
architectural decision record and this document does not revisit it.

Everything below that says "measured" was measured for this landing, against this tree. The three
findings worth reading even if you never touch Phase 0 are: **`bevy_app::App` is not `Send`**,
**bevy currently costs the browser bundle nothing**, and **`scripts/wasm-size.sh` cannot run on
`main`** for reasons that predate this work.

## How it works

`crates/lodestone-server/src/ecs/` is the whole implementation, four files:

| file | contents |
|---|---|
| `mod.rs` | `ServerApp`, the builder; module doc carries the design rationale |
| `plugin.rs` | `ServerCorePlugin`, `ServerTick`, `ServerTickWitness`, `advance_server_tick` |
| `schedules.rs` | `ServerBoot` / `NetIngest` / `GameTick` labels, `TickSet` / `IngestSet` sets |
| `gate.rs` | `#[cfg(test)]` — the production-reachability gate and its encodable control |

`ServerCorePlugin` is an ordinary `bevy_app` plugin: nothing in it is privileged, and a third party
could have written it. It installs the three schedules, chains `TickSet` inside `GameTick` and
`IngestSet` inside `NetIngest`, and registers exactly one system, `advance_server_tick`.

`ServerApp::bootstrap()` builds an `App::empty()`, adds the plugin, and runs `ServerBoot` once.
`IntegratedServer::open_in_memory_with_mobs` calls it in production — **synchronously**, before it
spawns the tick task, next to where `MobHandle::seeded` is already built for the same reason — and
moves the resulting `World` into that task.

### Why the tick task owns a `World` and not an `App`

`bevy_app::App` is **not `Send`**. Its `runner` field is `Box<dyn FnOnce(App) -> AppExit>` with no
`Send` bound (`bevy_app-0.19.0/src/app.rs:1537`), so an `App` cannot be moved into a `tokio::spawn`ed
future and cannot be held across an `.await` inside one. `World` *is* `Send`, and the `Schedules`
resource lives *in* the `World`, so a `World` moved out of a finished `App` still runs every schedule
the plugins installed. `ServerApp::into_world()` is that move; `ecs::tests::the_extracted_world_is_send`
is the compile-time assertion, and `the_extracted_world_still_runs_its_schedules` is the runtime one.

This happens to be exactly the phrasing `docs/server-ecs.md` already used — "the server's `World` is
held directly by the tick task" — so Phase 0 changes no decision. It supplies the mechanical reason it
could not have been otherwise, which matters for the next phase:

> **Phase 1 should thread `&mut World` into `crate::tick::run_tick_loop`, not `&mut App`.** The plan's
> Phase 0 text says `app.world_mut().run_schedule(GameTick)` and `run_tick_loop` gaining
> `app: &mut App`; the second half does not compile behind `crate::spawn::spawn`.

### Why not `lodestone-ecs`

`docs/server-ecs.md`'s title says "link `lodestone-ecs` into `lodestone-server`", and Phase 0
deliberately does not. Two `World`s means a shared `ScheduleLabel` *type* buys nothing at runtime — a
label is a key into one `World`'s `Schedules`, so the client's `GameTick` and the server's can never be
the same `Schedule` value even with the same label type. What sharing would buy is one import path for
a plugin author; what it would cost is the entire client vocabulary (`LocalPlayer`, `FrameClock`,
`SessionMenus`) plus `lodestone-physics`/`-game`/`-world` landing in this crate's graph and in the
browser bundle. The decision record already names the substrate/client-vocabulary split as the fix and
already calls it a **follow-up, not a prerequisite**.

Consequence to accept knowingly: `NetIngest`, `GameTick` and `TickSet` now exist in two crates with the
same names. When the split lands, `crate::ecs`'s versions become re-exports of the substrate crate's and
no plugin has to change the set it names.

### Never install `CorePlugin` on a server `App`

`lodestone_ecs::CorePlugin` inserts three resources, and the decision record's gotcha lists two:

| resource | verdict |
|---|---|
| `WorldTime` | reusable in principle, but arrives welded to the other two |
| `FrameClock` | a lie — there is no frame, and open-to-LAN has no render loop at all |
| `LockHolds` | **worse than a lie** — the meter for a lock the server does not have, so a reading of zero would look like a measurement |

It also configures `FrameSet::{Input, Interpolate, Camera, Terrain}` into `Update`. `Update` does not
exist on an `App::empty()`-built server `App`, and **`configure_sets` creates a schedule that is
absent** — so installing `CorePlugin` would not fail loudly, it would quietly grow a frame-shaped
schedule inside the server.

## The gate, and the controls that were actually run

Phase 0's plan text says it is "deliberately an island for exactly one phase". It is not, and that was
the one hard requirement: `WindowApp.ecs` on the client (issue
[#37](https://github.com/matteopolak/lodestone/issues/37)) is an `App` constructed and never run
against — "an inert scaffold nothing reads", still open. Constructing an `App` and stopping is that
defect verbatim.

So `advance_server_tick` increments `ServerTick` inside the `World` *and* mirrors it onto
`ServerTickWitness`, an `Arc<AtomicU64>` the holder can only read.
`IntegratedServer::server_tick_count()` reads it back. That accessor is a **metric, not a back door**:
no simulation state travels through it, nothing branches on it, and it hands out no reference into the
`World` — the no-lock invariant survives.

`ecs::gate::the_production_integrated_server_runs_a_registered_system` drives the real
`IntegratedServer::open_in_memory_with_mobs` and asserts `Some(1)` exactly. Not `>= 1`, not `is_some`:
`Some(0)` is the island, `None` means production stopped building the `World`, and a value above 1 means
something ran the schedule twice (or Phase 1 landed and the gate needs to account for `GameTick`).
It needs no polling — bootstrap is synchronous, so the assertion is deterministic under any load.

### Negative controls, observed

Both were run and both failed as required. Neither is a description of what would happen.

| control | edit | observed |
|---|---|---|
| the schedule run | removed `app.world_mut().run_schedule(ServerBoot)` from `ServerApp::bootstrap` | gate failed, `left: Some(0)` / `right: Some(1)` — the island signature; 3 other tests failed with it |
| the production wiring | `server_tick: Some(server_tick)` → `None` in `integrated.rs` | gate failed, `left: None` / `right: Some(1)` |

Two more controls are encoded and pass as tests: `a_constructor_with_no_tick_task_reports_no_world`
(`open_in_memory` builds no `World`, so the accessor must answer `None` — proving the gate distinguishes
wired from unwired) and `a_world_without_the_plugin_leaves_the_witness_at_zero` (running the same
schedule labels with no registered system must not move the witness — proving the witness reports a
system *executing*, not a schedule being run).

### A control whose premise was false, caught by running it

`the_server_app_has_no_frame_shaped_schedule` asserts the server `App` has neither `Update` nor `Main`.
Its control first asserted that installing `MainSchedulePlugin` creates `Update`. **It does not** —
`bevy_app-0.19.0/src/main_schedule.rs:311` adds `Main`, `FixedMain` and `RunFixedMainLoop` and never
touches `Update`, which `App::default()` gets only because its own later `add_systems`/`configure_sets`
calls create it on demand. The control failed with "MainSchedulePlugin did not create `Update`". This is
CLAUDE.md's *premise-false* hazard, and the reason the rule is to run a control rather than describe
one. Both halves are now driven by what actually creates each schedule: `MainSchedulePlugin` for `Main`,
a `configure_sets(Update, …)` call for `Update`.

### Ambiguity detection

`every_server_schedule_initializes_under_strict_ambiguity_detection` builds all three schedules under
`ambiguity_detection: LogLevel::Error`; `a_second_unordered_server_tick_writer_fails_the_ambiguity_check`
is its control and reports the rogue writer. Both copy
`lodestone_controller::ecs::exactly_one_system_writes_movement_intent`'s recorded gotcha verbatim: **do
not run the app first.** An already-built schedule is not rebuilt, so `initialize` returns `Ok` without
consulting the new settings, which is exactly how the assertion goes vacuous.

Note `advance_server_tick` takes `Res<ServerTickWitness>`, not `ResMut` — the witness is an
interior-mutable counter, so taking it immutably keeps a second registered system from being an
ambiguity against this one on that resource. `ServerTick` is the `ResMut`, and it is what the control
above collides on.

## Binary size, measured

### `scripts/wasm-size.sh` cannot run on committed `main`, for two pre-existing reasons

Both are committed and neither is caused by this work. `just wasm-size` fails at
`error: release build failed`, which hides them; they surface only from a direct
`cargo build --release --target wasm32-unknown-unknown` in `web/`:

1. **`getrandom 0.2` without its `js` feature.** `lodestone-v770` → `rand 0.8` → `rand_core 0.6` →
   `getrandom 0.2.17`, whose `compile_error!` fires on `wasm32-unknown-unknown`. `web/Cargo.toml` and
   `crates/protocol/v770/Cargo.toml` are both unmodified in the working tree, and the lockfile entries
   are committed. This also fails `scripts/wasm-check.sh`'s `lodestone-v770` row and its `trunk build`
   row. Standard fix: `getrandom = { version = "0.2", features = ["js"] }` in `web/Cargo.toml`.
2. **`web/src/main.rs` has drifted from `lodestone-render`'s camera API.** `block::camera_buffer` no
   longer exists and `BlockPipeline::camera_bind_group` now takes a third `origin_buffer` argument
   (`crates/lodestone-render/src/block.rs:358`). This one is masked by the first: `getrandom` fails
   earlier in the graph, so `wasm-check`'s output never names it.

Both are reported rather than worked around. Fixing a churning render crate's browser consumer is not
Phase 0's scope, and a fix invented here could change the very number being measured.

### The number, measured another way

Measured with the plan's own methodology — throwaway `wasm32-unknown-unknown` bin crates outside the
repo, `web/`'s exact release profile (`opt-level = "z"`, `lto = "fat"`, `codegen-units = 1`,
`panic = "abort"`, `strip = true`), all three linking `lodestone-server` at the same base commit
(`feb618c`) so bevy is the only variable:

| variant | raw | gzip |
|---|---|---|
| A′ — `lodestone-server` with **no** bevy dependency | 334,087 B | 77,191 B |
| A — bevy declared, `crate::ecs` compiled but never called | 334,004 B | 77,145 B |
| B — `ServerApp::bootstrap()` + one `GameTick` run | 632,883 B | 187,223 B |
| **A′ → A (what the browser bundle pays today)** | **−83 B** | **−46 B** |
| **A → B (the deferred cost)** | **+298,879 B (+292 KiB)** | **+110,078 B (+107 KiB)** |

**Today's cost to the shipping browser bundle is zero** — measurably below noise, and negative by a few
dozen bytes, which is link-layout jitter and not a saving. The reason is structural, not luck:
`open_in_memory_with_mobs` is `#[cfg(not(target_arch = "wasm32"))]`, so nothing on wasm32 constructs a
`ServerApp`, and `lto = "fat"` eliminates the module wholesale.

The deferred cost is real and lands the moment a browser build constructs the `World`. At +107 KiB gzip
against `wasm-size.sh`'s 1,600,000 B ceiling and its recorded 1.21–1.24 MiB baseline, headroom goes from
~20–25% to roughly 10–13%. It fits and it eats about half the remaining margin — which corroborates the
plan's `+352 KiB / +130 KiB` estimate at slightly lower magnitude, exactly the direction the plan
predicted when it called its own figure an over-estimate (a real build shares allocator and panic
machinery).

**So the gate to re-run is Phase 1's, not Phase 0's**, and it should be re-run the day browser
singleplayer gains a tick loop.

## How to change it, and the gotchas

- **Do not delete the witness while the `World` is still shallow.** `ServerTickWitness` and
  `server_tick_count()` are the only thing separating this from issue #37. Once Phase 1 lands they get
  *stronger*, not redundant: the witness must then advance in lockstep with `TickStats::tick_count`, and
  a divergence is the island detector the plan's Phase 1 asks for.
- **Update the gate's predicted value when Phase 1 lands.** `Some(1)` is correct only while `ServerBoot`
  is the only schedule production runs. Phase 1 makes it advance per tick; change it to a lockstep
  assertion against `tick_stats()`, not to `>= 1`.
- **Assert against the production-built `World`, never a hand-built one.** A test that builds its own
  `App` passes whether or not production wires anything.
- **Nothing may hand out a reference into the `World`.** The no-lock invariant holds because there is no
  accessor, not by convention. Publish a snapshot from `TickSet::Publish` the way `LiveMobSource` does.
- **`crate::ecs` needs its `lib.rs` line to exist at all.** `lib.rs` is a brokered choke point; Phase 0's
  patch to it is `pub mod ecs;`, one line. Without it the module is four files cargo never compiles, and
  `cargo check` is green — which is exactly how a five-line omission left 2,666 lines of redstone dead.
  `pub` rather than plain `mod` deliberately: the re-exports would otherwise be unreachable and warn.
- **No bench yet, and that is deliberate.** The plan assigns `crates/lodestone-server/benches/world_tick.rs`
  to Phase 1, measuring **one full `run_tick_loop` iteration** — a scene Phase 0 does not have. A bench
  pointed at `ServerApp::bootstrap()` instead would be CLAUDE.md's *world* species: exemplary code aimed
  at something that structurally cannot exercise the change Phase 7 needs an anchor for. It also cannot
  be committed green before the brokered `lib.rs` line lands, since a bench compiles against the public
  API.

## Configuration

None. There is no server-side plugin-loading mechanism, feature flag or manifest yet — a plugin today is
a `Cargo.toml` dependency added with `App::add_plugins`.

## Dependencies

`bevy_app` and `bevy_ecs` 0.19, through the same `[workspace.dependencies]` entries `lodestone-ecs`
builds against (`Cargo.toml:91-92`): `default-features = false, features = ["std"]`, so no
`bevy_reflect` and never `multi_threaded`. That last omission is what keeps this migration free of a
second threading model, and it is why bevy dispatches these systems as direct calls rather than through
a task pool.

The plan's Phase 0 also called for promoting `lodestone-world` and `lodestone-game` out of
`[dev-dependencies]`. Not done: nothing in Phase 0 names either, and an unused dependency edge is
compile cost for every consumer with no reader. Phase 1's state migration is where they earn the
promotion.

## See also

- [`docs/server-ecs.md`](./server-ecs.md) — the decision record. Read it first.
- [`docs/plans/server-ecs-migration.md`](./plans/server-ecs-migration.md) — the phased plan.
- [`docs/plugin-api.md`](./plugin-api.md) — the five-clause client-side doctrine, clause 4 of which
  inverts server-side: the remote client's input is a proposal, and the plugin is entitled to overrule
  it. `TickSet::Adjudicate` is where that becomes expressible.
- [`docs/server-tick-loop.md`](./server-tick-loop.md) — the loop Phase 1 threads a schedule through.
- Issue [#433](https://github.com/matteopolak/lodestone/issues/433) — the migration.
- Issue [#37](https://github.com/matteopolak/lodestone/issues/37) — the client-side inert `App` this
  phase exists not to reproduce.
