# Autonomous navigation: `lodestone-nav` + `lodestone-autopilot`

## What it is

Two crates under [`crates/plugins/`](../crates/plugins/) implementing a Baritone-class client-side
pathfinder: `lodestone-nav` is a version-free search core (`(snapshot, start, goal, policy, budget) →
plan`), and `lodestone-autopilot` is the bevy plugin that wraps it into `MovementIntent`/`LookIntent`
output. It is a separate subsystem from server-side mob AI (`docs/mob-ai.md`) — this is a pathfinder
for a *player-shaped* body, built as an opt-in plugin for people writing bots on top of the client, not
a feature of the shipped game.

## How it works

`lodestone-nav` is a plain library — no bevy, no ECS, no threads — so it can be tested headlessly
against a fixture world or real jar-derived collision data with nothing between the oracle and the
code under test. It implements the real-terrain movement kinds `Walk`, `StepUp`, `Descend`, `Drop`,
`WalkDiagonal` and `Climb`, plus corner-cutting rules (a diagonal step is refused unless both
orthogonal "shoulder" cells are clear, matching vanilla's mob-pathfinder diagonal check) and
segmentation, so a goal further than one search snapshot away is planned as a chain of legs rather than
failing outright. Breaking and placing blocks as part of a route are not implemented.

`lodestone-autopilot` bridges the search core into the ECS with three resources —
`AutopilotGoal` (the public control surface: `Some(BlockPos)` starts or retargets a search, `None`
stands down), `AutopilotStatus` (`Idle`/`Planning`/`Driving`/`Failed`/`Arrived`), and a private
`AutopilotState` holding the in-flight plan — and two systems, `plan_route` and `drive_plan`, chained
`.after(TickSet::Intent).before(TickSet::Physics)`. `plan_route` steps a resumable search a bounded
number of nodes per tick so it never blocks a frame; `drive_plan` turns the current plan edge into a
`MovementIntent`/`LookIntent` via `WalkDrive`/`ClimbDrive`, closed-loop against the player's actual
`PlayerState` every tick. A debug-only `extract_plan_billboards` system draws the live plan as a trail
of markers through the same extract-time billboard channel other plugins use.

**Segmentation.** Once the active plan's remaining cost (excluding the edge currently executing) drops
below a lead-tick threshold, a second search is dispatched from the plan's terminal node; when the
active plan's edges run out, the continuation splices in as the new plan at edge zero. If nothing is
ready yet, the executor holds still rather than mis-splice.

**Witness invalidation.** A committed plan is not trusted forever: every tick, a small look-ahead
window of upcoming edges is diffed against the live world by block-state id, and a slower full sweep
of the plan's remaining edges runs on a longer interval to catch a change further down the route. A
mismatch — including a chunk that unloaded out from under the plan — discards the plan and its
continuation and forces a fresh search from the player's live position. Only raw block-state identity
is compared, not re-derived legality; per-edge *cost* re-verification (a cell unchanged but now more
expensive, e.g. a mob in the way) is not modelled, since this crate has no mob-avoidance or per-tick
fluid model to check it against.

**Costing.** Every movement kind is priced by simulating it against a synthetic collision frame once
and caching the result. Two things worth knowing when reading these numbers: `WalkDrive::done()`/
`arrived()` is a *cell-boundary* crossing test, not a centre-to-centre distance, so measured costs
differ from naive geometric estimates — a diagonal step measures roughly 1.17x a straight step from a
"straight" entry and roughly 0.89x (genuinely cheaper) from a "reverse" entry, not the ~1.41x a plain
Euclidean estimate would suggest. And climbing up is slower than climbing down (~8.5 ticks/block vs.
~6.67), the opposite of a naive equal-rate assumption, because gravity is subtracted from the upward
climb target before it reaches the mover but the downward clamp is a floor applied directly to
velocity.

## How to change it, and the gotchas

- `WalkDrive` (the executor) aims at the destination cell centre on both axes and only varies by a
  single `jump: bool` flag — adding `StepUp`/`WalkDiagonal` needed no executor changes at all. `Climb`
  is the exception: it needs its own `ClimbDrive` (holds jump to ascend, holds nothing to descend) and
  its own vertical-only collision frame, because it has no floor and no horizontal displacement to
  reuse `WalkDrive`'s frame for.
- A move whose source and destination surfaces differ in height needs a same-height check in
  `arrived()`, not just a horizontal-cell + `on_ground` check — the player's AABB still overlaps the
  source column for a few ticks after crossing the boundary, so a plain horizontal check can report a
  multi-block drop as "arrived" before it has fallen at all. Any future `MoveKind` whose surfaces differ
  in height (or later, fluid state) should assume this trap exists until proven otherwise.
- A climbable block (ladder, vine) has a real, non-full collision shape but does not "block motion" in
  the census sense — treat it as air for support/head-room purposes (never a floor to stand on, in, or
  under) while keeping its real shape for physics and its climbable fact for movement legality. Getting
  this wrong refuses mounting a ladder outright, or lets a body stand on top of one, or blocks a climb
  chain from ever reaching its own bottom rung.
- `fall_step` unifies `Descend` and `Drop` into one legality function: a falling body stops at the
  first surface it reaches, so there is no family of "try landing N cells down" variants.
- A search-graph legality gate that reads the cell *below* the stand position (a support check) must
  not run for a body whose feet rest *inside* a partial block (slab, soul sand, snow layer) — that
  block already is the cell being checked, and reading one cell further down looks past a hazard the
  block itself would seal the player from, or refuses a stand at the bottom of a loaded snapshot where
  "one below" is out of range.
- `lodestone-autopilot` never touches `ActionQueue` directly — it only ever produces
  `MovementIntent`/`LookIntent`, and it is `player_physics` plus `send_move_action` downstream that put
  anything on the wire, regardless of who drove the intent that tick.
- `plan_route` steps a `Search` incrementally across ticks; do not simplify it to a single blocking
  `Search::run` call — that reintroduces the frame stall the budgeted stepping exists to avoid.
  `lodestone_nav::drive::compute_plan` is the sanctioned blocking entry point for a caller (a test, or
  an offline tool) that actually wants run-to-completion behaviour.
- A plugin crate that derives `Resource`/`Component` needs `bevy_ecs`/`bevy_app` as direct dependencies,
  not only `lodestone-ecs` — bevy's derive macros emit absolute `bevy_ecs::` paths.
- Hermetic tests hand-build a `World` fixture and a minimal `VersionAdapter` independently of the
  physics-side collision fixture — production reads `ChunkWorld` (planning) and `PlayerCollision`
  (physics) through two different seams, and collapsing them in a test would hide a plugin accidentally
  depending on one standing in for the other. A flat, all-full-cube fixture also cannot exercise a
  partial-block shape (a slab, a ladder) — prefer a real jar-derived collision census for anything that
  needs to prove a specific shape is handled correctly, not just that a walk arrives.

**Not wired into the shipped client.** `lodestone-shell` does not depend on `lodestone-autopilot` —
there is no cargo feature and no chat command to drive it. It is a pre-implemented external plugin for
people building on the library. To use it: build your own `lodestone_ecs::app::App` with
`AutopilotPlugin` registered and hand its `World` to `lodestone_client::ClientBuilder::ecs` (the route
`tests/drives_to_goal.rs` itself uses), or register `AutopilotPlugin` into the rendered client via
`Sim::client_app()` if you want a window as well.
`lodestone_app::client_app()` composes the same plugin set the shipped client runs, which matters:
`ControllerPlugin` writes `MovementIntent` one tick-set before the autopilot does, so a bot assembled
from a smaller ad hoc plugin stack can pass its own tests and still lose every tick to the controller
once run against the real set.

## Configuration

`AutopilotGoal` is the only public runtime control surface. `SNAPSHOT_RADIUS` (currently 8 columns,
~143 blocks from the search's start in any direction) is a compile-time constant, not a runtime policy
knob, and also bounds how far a plan can get before segmentation must dispatch a continuation.
`NavPolicy::default()` governs everything else the search considers — `max_fall_blocks` (`Drop`'s
legality cap), `jump_penalty`, `damage_cost`, and `replan_lead_ticks` (default 30 ticks) — and nothing
in the plugin yet exposes these as runtime knobs; every call site uses the default policy.

## Dependencies

- `lodestone-nav` — the search core.
- `lodestone-ecs` — `ChunkWorld`, `VersionData`, `TickSet`, `MovementIntent`, `LookIntent`,
  `LocalPlayer`, `GameTick`.
- `lodestone-model` — `BlockPos`, and (through `VersionData`) `VersionAdapter`.
- `lodestone-physics` — the physics profile the cost model's simulation runs against.
- `lodestone-world` — the `World` type `ChunkWorld::read()` guards.
- `bevy_ecs` / `bevy_app`, direct.
- `lodestone-data`, dev-only — real per-block-state collision/name/solidity data for integration tests.

## See also

- [`docs/plugin-api.md`](./plugin-api.md) — the plugin surface this crate consumes.
- [`crates/plugins/README.md`](../crates/plugins/README.md) — licensing and ownership for everything
  under `crates/plugins/`.
