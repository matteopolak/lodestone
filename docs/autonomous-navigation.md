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
- **Not registered into the shipped client.** `lodestone_shell::sim::Sim::new`
  (`crates/lodestone-shell/src/sim.rs`) is where the real `App` is built and every engine/plugin is
  added (`app.add_plugins((CorePlugin, LocalPlayerPlugin, ControllerPlugin, …))`). `AutopilotPlugin`
  is not in that list. Until it is, the plugin is a correct, tested island in exactly `CLAUDE.md`
  rule 1's sense: built, proven against the real `player_physics` seam in a hermetic test, and
  reaching zero players because nothing in the running client constructs it. `sim.rs` is outside this
  change's file ownership (a different agent's cluster as of this writing) — the patch below is what
  its owner needs to apply.

  ```diff
  --- a/crates/lodestone-shell/src/sim.rs
  +++ b/crates/lodestone-shell/src/sim.rs
  @@ let mut app = lodestone_ecs::app::App::new();
       app.add_plugins((
           CorePlugin,
           LocalPlayerPlugin,
           ControllerPlugin,
           SessionHudPlugin,
           lodestone_ecs::ingest::IngestPlugin,
           lodestone_ecs::SessionPlugin,
           crate::entities::EntityInterpPlugin,
           TerrainPlugin,
           InteractPlugin,
  +        // Autonomous navigation (docs/autonomous-navigation.md, issue #38): the
  +        // M1 walk-only plugin. Adds no systems that fire without an
  +        // `AutopilotGoal` set, so this is inert for every session until
  +        // something (a chat command — not yet built either, see the doc)
  +        // sets one.
  +        lodestone_autopilot::AutopilotPlugin,
       ));
  ```

  and a `lodestone-autopilot = { workspace = true }` line in `crates/lodestone-shell/Cargo.toml`'s
  `[dependencies]`. Both are small and mechanical; they were not applied here because `sim.rs` is
  contended (`CLAUDE.md`'s repo-hazards note) and out of this change's ownership, not because either
  is technically hard.

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

## See also

- [`docs/plugin-api.md`](./plugin-api.md) — the plugin surface this crate consumes: what a
  `TickSet::Intent` system can read and write, and nothing this plugin uses that a different
  third-party plugin could not also reach.
- [`docs/baritone-port.md`](./baritone-port.md) — the full design this is M1 of; its §3.7 and §7 list
  what was missing when it was written and what has since closed.
- [`crates/plugins/README.md`](../crates/plugins/README.md) — the licensing and ownership rules for
  everything under `crates/plugins/`.
