# Autonomous navigation: `lodestone-nav` + `lodestone-autopilot`

## What it is

Two crates under [`crates/plugins/`](../crates/plugins/) implementing M1 and part of M2 of
[`docs/baritone-port.md`](./baritone-port.md)'s Baritone-class navigation design:

- [`lodestone-nav`](../crates/plugins/lodestone-nav) — the version-free search core. A plain library:
  no bevy, no ECS, no threads. `(snapshot, start, goal, policy, budget) → plan`, plus `WalkDrive`,
  "given a plan edge and a `PlayerState`, what keys do I press this tick".
- [`lodestone-autopilot`](../crates/plugins/lodestone-autopilot) — the bevy plugin wrapping it: a
  goal resource in, `MovementIntent`/`LookIntent` components out, through the exact same seam
  documented in [`docs/plugin-api.md`](./plugin-api.md).

**Where this stands, honestly:** `lodestone-nav` implements `Walk`, `StepUp`, `Descend` and `Drop`
(M2's real-terrain kinds bar diagonals and climbing — see §"M2, so far" below) plus segmentation
(a journey longer than one snapshot no longer stalls at the boundary). `WalkDiagonal` and `Climb`
are **not** implemented; breaking and placing are M4/M5. Point the plugin at a reachable block —
now including one a block or two up, down, or off a short drop — and it walks, climbs or falls
there, planning the next leg while still walking the current one if the goal is further than one
snapshot away. That is still deliberately not a finished bot.

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
| `AutopilotState` (private) | `lodestone-autopilot/src/lib.rs` | The in-flight `Search`/`Plan`, current edge index, `reached_goal` (whether `plan` already ends inside the goal), and — M2's segmentation addition — a second `Search`/`Plan` pair (`continuation_search`/`continuation_plan`) for the *next* leg, plus `continuation_reached_goal`. |
| `plan_route` | `lodestone-autopilot/src/lib.rs` | Steps a resumable `Search` one `Budget::PER_TICK` (2,000 nodes) at a time — never blocks a frame, per `docs/baritone-port.md` §2.2(2)'s "no chunks while stalled" rule. Builds the `FactsTable` from `Res<VersionData>` via `AdapterCensus`; refuses (reports `FailReason::NoVersionAdapter`) rather than guessing when no adapter is compiled in, matching `FactsTable::empty()`'s own documented policy. Since M2, also dispatches and steps the *continuation* search once the active plan's `remaining_cost_after` the current edge drops below `NavPolicy::replan_lead_ticks` — see "Segmentation" below. |
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

### Segmentation (M2): the next leg is planned while the current one is still being walked

`docs/baritone-port.md` §4.9. Before this, `lodestone-nav`'s own `min_progress`/tail-truncation
machinery already returned a "best partial" for a goal outside one snapshot, but the *plugin* threw
it away — any non-`Reached` search outcome hard-failed via `FailReason::Search(outcome)`, matching
M1's documented "one search dispatch (no segmentation)" scope. A goal further than
`SNAPSHOT_RADIUS * 16 + 15` blocks (143, at the default radius of 8) was therefore simply
unreachable, full stop.

M2 changes two things in `plan_route`:

1. `Outcome::BudgetExhausted`/`Outcome::WorldExhausted` with a plan clearing `min_progress` are now
   *driven*, exactly like `Reached` — `AutopilotState::reached_goal` records which case it was, since
   only the goal-missing one ever needs a continuation.
2. Once `plan.remaining_cost_after(state.edge)` — **excluding the edge currently executing**, so one
   long `Drop` cannot suppress this forever — drops under `NavPolicy::replan_lead_ticks` (default 30
   ticks / 1.5 s), a second `Search` is dispatched from `plan.terminal()`: same position, same
   `Arrival`. `lodestone_autopilot::drive::continuation_search` is the constructor; it shares
   `search_from` with `seed_search`, so a continuation is indistinguishable from a fresh search to
   everything downstream of it.

`drive_plan` splices the continuation in the instant the active plan's edges run out
(`plan.edges().get(state.edge)` returns `None`): if `continuation_plan` is `Some`, it becomes the new
`plan` at `edge = 0` with no stutter — concatenation is valid by construction, since the
continuation started at exactly this plan's terminal node. If nothing is ready yet, the executor
holds still (`MovementIntent::NONE`) rather than mis-splice or guess, per §4.9's own preference for a
visible pause over a wrong move. There is no rate limit on repeated continuation-dispatch attempts
yet (`NavPolicy::min_replan_interval_ticks` exists but is not wired to this path) — acceptable for
"medium journeys work", worth revisiting if a goal that is unreachable *past* the snapshot edge is
found to spin.

**What is deliberately not built**: witness-set invalidation, per-edge re-verification, the
look-ahead window, prefix trimming, and early adoption (all `docs/baritone-port.md` §4.5/§4.9 items)
are still open. Segmentation here is "the mechanism that lets a plan longer than one snapshot exist
and be driven end to end", not the full executor-robustness story M2's milestone description also
names.

**Gate**: `tests/drives_to_goal.rs`'s `a_goal_beyond_the_first_snapshot_is_reached_by_splicing_a_continuation`
sends the goal to `x = 200` while a single search's view caps out at `x = 143` — `Arrived` is only
reachable through this test if a second search actually ran and its plan was actually spliced on.

### What it does not do yet

- **No chat command from this crate.** `docs/baritone-port.md` §9's M1 milestone names `#goto x z`.
  This crate still routes nothing itself — but `lodestone-shell` has since wired one up
  (`crates/lodestone-shell/src/sim.rs`'s `#goto` handling, gated by
  `sim::tests::goto_chat_command_drives_the_player_toward_the_goal_over_real_ticks`), outside this
  crate's ownership, so the milestone's stated observable is live even though the plumbing is not
  here.
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

- **`MoveKind` has four variants now — `Walk`, `StepUp`, `Descend`, `Drop(Dir4, n)` — and the M1
  forcing function already did its job once.** `drive::edge_drive` used to have
  `let lodestone_nav::MoveKind::Walk(_) = edge.kind;`, an irrefutable-pattern assertion that stopped
  compiling the moment a second kind landed, forcing a real `match` rather than a silent mis-handle.
  The answer for all three M2 additions turned out to be "no new script needed" — `WalkDrive` already
  aims at the destination cell centre and brakes-or-doesn't identically for all four; the only
  physical difference is `WalkDrive::jump`, a plain bool set for `StepUp` only. `WalkDiagonal` and
  `Climb` are **not implemented** (M2's own scope stopped short of them this pass — see
  `docs/baritone-port.md` §9's M2 status note); `Climb` in particular will need a real second script
  (holding a direction key against a ladder, not aiming at a cell centre), which is the next thing
  that should grow `edge_drive`'s `match` for real.
- **`fall_step` (M2) unifies `Descend` and `Drop` into one legality function, not two, because a
  falling body stops at the first surface it reaches.** There is never a family of "try landing 2, 3,
  4 cells down" — see `graph.rs`'s own doc comment on `fall_step` for the reasoning and the hazard
  and slab-exclusion rules layered on top of it.
- **`WalkDrive::done()` used to check only horizontal cell + `on_ground`, and that is unsafe the
  moment source and destination surfaces differ.** The player's AABB is 0.6 wide, so a body whose
  *centre* has just crossed a cell boundary still overlaps the source column for a few ticks. For
  `Walk`, where both surfaces are the same height, that overlap is harmless. For `StepUp`/`Descend`/
  `Drop` it produced a real, silent bug: a synthetic `Drop` of 2 cells and one of 6 both "completed"
  in identically 4.93 ticks, at `y` never having left the *source* surface at all — `done()` fired on
  the horizontal coincidence before the fall (or climb) ever happened, and the cost model measured a
  walk that never occurred. Fixed by `WalkDrive::arrived()`, which adds a same-height check
  (`(position.y - surface).abs() < SURFACE_ARRIVAL_EPS`, 0.1 blocks — `docs/baritone-port.md` §4.8's
  own arrival-tolerance figure) and is used in both `done()` and the jump-input gate in `tick()`. This
  was found by the M2 cost tests themselves, not by inspection — see `cost.rs`'s
  `a_longer_drop_costs_more_ticks_than_a_shorter_one` and its history for the exact numbers. Any
  future `MoveKind` whose source and destination differ in height (or, later, in fluid state) should
  assume the same straddle trap exists until proven otherwise.
- **`jump_apex_height()` (`graph.rs`) is derived by simulation, cached in a `OnceLock`, not a literal
  transcribed from `docs/baritone-port.md` §4.3.** It runs one jump against a synthetic flat floor and
  measures the highest point reached; `StepUp`'s legality gate (`delta` must exceed `STEP_HEIGHT` but
  not exceed this) is the number that decides a 1.0 ascend clears and a 1.5 one does not, matching
  §4.3's own worked example.
- **`TemplateKey` carries a `drop_n: u8` field, separate from `MoveKind::id()`.** A drop of 2 cells and
  a drop of 6 are not the same equivalence class — they take genuinely different tick counts — and
  `MoveKind::id()` alone cannot distinguish them (it is a *dense* id, deliberately small, shared by
  every direction and every `n`). Folding `n` into `id()` instead would have memoised the *first*
  simulated drop height and silently cost every other one its ticks — exactly the "search believes 6,
  executor needs 14" failure `docs/baritone-port.md` §4.4 exists to make impossible. If a future kind
  ever needs a second continuous parameter, it goes in its own `TemplateKey` field, not into `id()`.
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

`AutopilotGoal` is the only public control surface, set by inserting the resource (a test) or a
chat command (`lodestone-shell`'s `#goto`, outside this crate). `SNAPSHOT_RADIUS`
(`lodestone-autopilot/src/lib.rs`, currently 8 columns in every direction — 143 blocks in any one
direction from the search's start) is a compile-time constant, not yet a runtime policy knob; it is
also, since M2, the number that decides how *far* a plan can get before segmentation has to dispatch
a continuation. `NavPolicy::default()` governs everything the search itself considers — `max_fall_blocks`
(`Drop`'s legality cap, default vanilla's own `SAFE_FALL_DISTANCE` — zero damage by default),
`jump_penalty`, `damage_cost`, and, for segmentation specifically, `replan_lead_ticks` (default 30) —
and the plugin does not yet expose any of it as a runtime knob either; every call site uses
`NavPolicy::default()`.

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
