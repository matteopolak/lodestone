# Autonomous navigation: `lodestone-nav` + `lodestone-autopilot`

## What it is

Two crates under [`crates/plugins/`](../crates/plugins/) implementing the M1 slice of
[`docs/baritone-port.md`](./baritone-port.md)'s Baritone-class navigation design:

- [`lodestone-nav`](../crates/plugins/lodestone-nav) — the version-free search core. A plain library:
  no bevy, no ECS, no threads. `(snapshot, start, goal, policy, budget) → plan`, plus `WalkDrive`,
  "given a plan edge and a `PlayerState`, what keys do I press this tick".
- [`lodestone-autopilot`](../crates/plugins/lodestone-autopilot) — the bevy plugin wrapping it: a
  goal resource in, `MovementIntent`/`LookIntent` components out, through the exact same seam
  documented in [`docs/plugin-api.md`](./plugin-api.md).

**M1 only, honestly stated:** `lodestone-nav` implements `Walk` and nothing else (no diagonals,
step-ups, drops, climbing, breaking or placing — those are M2+). Point the plugin at a reachable
block on flat-ish ground and it walks there. That is deliberately not a finished bot.

## How it works

### The two-crate split, and why the plugin is thin

`lodestone-nav`'s reason to be a plain library is that it can be gated headlessly against a fixture
world or a live server with no schedule between the oracle and the code being validated
(`docs/baritone-port.md` §4.0). `lodestone-autopilot` exists only to bridge it into the ECS: a
resource in, two components out, nothing else. The bridging code is deliberately factored into plain
functions (`crates/plugins/lodestone-autopilot/src/drive.rs`) rather than inlined into the systems, so
the same glue that a `GameTick` system calls per tick is also callable from a hermetic test with no
`bevy_ecs::World` at all.

### The resources and systems

| item | file:line (approx, `sim.rs`-adjacent files churn less) | what |
|---|---|---|
| `AutopilotGoal(pub Option<BlockPos>)` | `lodestone-autopilot/src/lib.rs` | Public control surface. `Some` starts (or retargets) a search; `None` stands down. |
| `AutopilotStatus` | `lodestone-autopilot/src/lib.rs` | Read-only: `Idle`, `Planning`, `Driving`, `Failed(FailReason)`, `Arrived`. |
| `AutopilotState` (private) | `lodestone-autopilot/src/lib.rs` | The in-flight `Search`/`Plan` and current edge index. |
| `plan_route` | `lodestone-autopilot/src/lib.rs` | Steps a resumable `Search` one `Budget::PER_TICK` (2,000 nodes) at a time — never blocks a frame, per `docs/baritone-port.md` §2.2(2)'s "no chunks while stalled" rule. Builds the `FactsTable` from `Res<VersionData>` via `AdapterCensus`; refuses (reports `FailReason::NoVersionAdapter`) rather than guessing when no adapter is compiled in, matching `FactsTable::empty()`'s own documented policy. |
| `drive_plan` | `lodestone-autopilot/src/lib.rs` | Turns the current plan edge into this tick's `MovementIntent`/`LookIntent` via `lodestone_nav::WalkDrive`, closed-loop (reads the player's *actual* `PlayerState` every tick, never a reference trajectory). |
| `AutopilotPlugin` | `lodestone-autopilot/src/lib.rs` | Registers the three resources and chains `(plan_route, drive_plan)` `.after(TickSet::Intent).before(TickSet::Physics)`. |

### Why `.after(TickSet::Intent)` and not `.in_set(TickSet::Intent)`

`TickSet::Intent`'s own doc comment (`crates/lodestone-ecs/src/sets.rs`) names both options: compose
inside the set with an explicit order against the specific human-input system, or override wholesale
by running strictly after the whole set. This plugin does the latter — it never has to name
`lodestone_controller::ecs::compute_movement_intent`, which matters because anchors are sets rather
than system functions specifically so a plugin does not have to track internal renames
(`docs/plugin-api.md`'s "how to change it"). The trade is that this plugin's intent always wins over
human input for whichever tick it writes something; there is no blending. That is the right default
for "an autopilot drives, a human doesn't fight it," and a future behaviour-arbitration layer
(`docs/baritone-port.md`'s "Arbitration" section) is where finer-grained handoff would live.

### Why this crate needs no `ActionQueue` access

`docs/plugin-api.md` documents `ActionQueue` as the sanctioned wire egress for a plugin. This plugin
never touches it: `MovementIntent`/`LookIntent` are consumed by `player_physics`
(`TickSet::Physics`), and *that* output — not the plugin's intent directly — is what
`lodestone_controller::ecs::send_move_action` (`TickSet::Send`) reports on the wire every tick,
regardless of who drove the intent that tick. Writing `ActionQueue` directly would be a second,
competing route to the same packet.

### What it does not do yet

- **No chat command.** `docs/baritone-port.md` §9's M1 milestone names `#goto x z`. Nothing routes
  chat input to `AutopilotGoal` — that plumbing lives in `lodestone-shell`, outside this change's
  ownership.
- **Now registered.** `lodestone_shell::sim::Sim::new`'s `app.add_plugins((CorePlugin,
  LocalPlayerPlugin, ControllerPlugin, …, InteractPlugin, lodestone_autopilot::AutopilotPlugin))`
  tuple has `AutopilotPlugin` in it, plus a `lodestone-autopilot = { workspace = true }` line in
  `crates/lodestone-shell/Cargo.toml`'s `[dependencies]`. Verified rather than assumed that the
  plugin's two systems (chained `.after(TickSet::Intent).before(TickSet::Physics)` internally, so
  registration order in the tuple does not matter) actually *run*, not merely that the plugin is in
  the list — `AutopilotStatus` defaults to `Idle` and nothing but `plan_route` can move it off that
  default, so `sim::tests::autopilot_plugin_is_registered_and_its_systems_actually_run` sets a goal,
  steps one tick, and asserts the status left `Idle`. `cargo check -p lodestone-shell
  --no-default-features` also stayed clean: none of `lodestone-autopilot`'s production dependencies
  (`lodestone-nav`, `lodestone-ecs`, `lodestone-model`, `lodestone-physics`, `lodestone-world`,
  `bevy_ecs`, `bevy_app`) is a version crate, so the workspace dependency does not compromise the
  version seam.

## How to change it, and the gotchas

- **`MoveKind` has exactly one variant (`Walk`) today.** `drive::edge_drive` has a
  `let lodestone_nav::MoveKind::Walk(_) = edge.kind;` line that is deliberately an irrefutable-pattern
  assertion: the moment `lodestone-nav` gains a second `MoveKind` (M2's `WalkDiagonal`/`StepUp`/…),
  this line stops compiling, which is the intended forcing function to add the second `match` arm
  rather than silently mis-handling the new kind as a walk.
- **A plugin crate that derives `Resource`/`Component` needs `bevy_ecs`/`bevy_app` as *direct*
  dependencies**, not only `lodestone-ecs`. See `docs/plugin-api.md`'s "how to change it" section for
  why (bevy's derive macros emit absolute `bevy_ecs::` paths) — `Cargo.toml` here is the worked
  example.
- **`plan_route` steps a `Search` across ticks; do not "simplify" it to `Search::run` in one call.**
  `lodestone_nav::drive::compute_plan` (a free function, also in `lodestone-autopilot`, re-exported)
  exists precisely so a caller that *does* want blocking, run-to-completion behaviour — a test, or a
  future offline "plan this route" tool — has a route to it without tempting the per-tick system into
  the same shortcut. Blocking the tick thread on a large search is exactly the stall
  `docs/baritone-port.md` §2.2(2) forbids.
- **The hermetic test (`tests/drives_to_goal.rs`) hand-builds both a `lodestone_world::World` fixture
  and a minimal `VersionAdapter` (`FixtureAdapter`).** The two are deliberately independent of each
  other and of `PlayerCollision`'s own fixture (`FlatFloor`) — production code reads `ChunkWorld` (for
  planning) and `PlayerCollision` (for physics) through two different seams, and collapsing them in a
  test would stop the test from being able to catch the plugin accidentally depending on one standing
  in for the other.
- **A flat, two-block synthetic census is a real gap in coverage, not a simplification.** `FixtureAdapter`
  only ever answers `AIR`/`STONE`, and every hand-built world in the original version of this test was a
  flat plane of full cubes — CLAUDE.md's "world" species of vacuous test, where the input structurally
  cannot exercise a non-cube shape. `tests/drives_to_goal.rs`'s `real_collision` module is the fix:
  a `RealDataAdapter` whose three census methods delegate straight to
  `lodestone_data::{collision_shapes, block_states, block_solidity}` — the same generated tables
  `lodestone_v770::adapter::V770Adapter` itself calls, dev-only (`Cargo.toml`'s `[dev-dependencies]`,
  not a version crate so this is not even the soft `SharedDependsOnVersion` isolation warning) — over a
  two-column real bottom `minecraft:oak_slab` floor (true collision top `0.5`) astride an otherwise real
  `minecraft:stone` path, and asserts the player's feet actually settle at `y ≈ 0.5` while crossing it,
  not just that the walk arrives (CLAUDE.md's "predict the value, not just the sign" discipline — a
  search or physics bug that quietly treated the slab as a full block would still let a 6-block walk
  *arrive*, so "arrived" alone was never the strong half of this test).
- **That gate found a real bug on its first run, which a flat-plane test structurally cannot.**
  `lodestone_nav::graph::standable`'s final hazard check —
  `if view.facts_at(x, y - 1, z)?.must_not_enter { return None; }` — ran unconditionally, on reasoning
  that only holds for [`stand_surface`]'s "below" branch (a full-cube support one cell under the stand,
  never swept by the body-hazard loop that starts at `y`). For the *other* branch — feet resting
  **inside** cell `y` on a partial block (a bottom slab, soul sand, farmland, a snow layer) — that block
  *is* cell `y`, already covered by the sweep, and `y - 1` is one cell further down than the stand
  depends on at all. Two bugs followed, both invisible to every fixture already in the tree (`flat()`'s
  `GridView` spans `-64..320`, so `y - 1` was always in range and never a hazard): a slab at the very
  bottom of a loaded `SnapshotView` reads `facts_at(x, y - 1, z)` as `None` (outside the snapshot), and
  the `?` propagated that into refusing an entirely ordinary stand; and a hazard one cell under a slab
  (lava under a soul-sand floor, say) would refuse standing on the block that fully seals the player
  from it. Fixed in `graph.rs` by gating that check on `(surface - f64::from(y)).abs() <= SURFACE_EPS`
  — true only for the "below" branch — with the reasoning recorded in `standable`'s own doc comment.
  All 75 pre-existing `lodestone-nav` unit tests still pass unchanged; none of them exercises this path,
  which is exactly why a genuine-jar-data, non-flat integration gate earns its cost even though the
  crate's own fixture-based unit suite is fast and already large.

## Configuration

None. `AutopilotGoal` is the only public control surface, set by inserting the resource (a test) or,
once built, a chat command. `SNAPSHOT_RADIUS` (`lodestone-autopilot/src/lib.rs`, currently 8 columns
in every direction) is a compile-time constant, not yet a runtime policy knob.

## Dependencies

- `lodestone-nav` — the search core.
- `lodestone-ecs` — `ChunkWorld`, `VersionData`, `TickSet`, `MovementIntent`, `LookIntent`,
  `LocalPlayer`, `GameTick`.
- `lodestone-model` — `BlockPos`, and (through `VersionData`'s public field) `VersionAdapter`.
- `lodestone-physics` — `PhysicsProfile::mc_1_21()`, the profile the search's cost model is simulated
  against.
- `lodestone-world` — the `World` type `ChunkWorld::read()` guards; `SnapshotView::build` takes it
  directly.
- `bevy_ecs` / `bevy_app`, direct (see "how to change it" above).
- `lodestone-data`, dev-only (`[dev-dependencies]`) — `tests/drives_to_goal.rs`'s `real_collision`
  module's source of genuine per-state jar-derived collision/name/solidity data. Not a version crate
  (it lives outside `crates/protocol/`), so this does not version-lock the plugin.

## See also

- [`docs/plugin-api.md`](./plugin-api.md) — the plugin surface this crate consumes: what a
  `TickSet::Intent` system can read and write, and nothing this plugin uses that a different
  third-party plugin could not also reach.
- [`docs/baritone-port.md`](./baritone-port.md) — the full design this is M1 of; its §3.7 and §7 list
  what was missing when it was written and what has since closed.
- [`crates/plugins/README.md`](../crates/plugins/README.md) — the licensing and ownership rules for
  everything under `crates/plugins/`.
