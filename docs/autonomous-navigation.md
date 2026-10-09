# Autonomous navigation: `lodestone-nav` + `lodestone-autopilot`

## What it is

Two crates under [`crates/plugins/`](../crates/plugins/) forming a Baritone-class client-side pathfinder for a player-shaped body: `lodestone-nav` is a version-free search core (`(snapshot, start, goal, policy, budget) -> plan`) and `lodestone-autopilot` is the bevy plugin wrapping it into `MovementIntent`/`LookIntent` output. It is an opt-in plugin for bot authors, separate from server-side mob AI ([Mob AI](./mob-ai.md)) and not wired into the shipped client.

## How it works

`lodestone-nav` is a plain library (no bevy, ECS or threads), testable headlessly against a fixture world or jar-derived collision data. It implements `Walk`, `StepUp`, `Descend`, `Drop`, `WalkDiagonal` and `Climb`, corner-cutting (a diagonal is refused unless both orthogonal shoulder cells are clear) and segmentation, so a distant goal is a chain of legs. Breaking and placing blocks are not implemented.

`lodestone-autopilot` has three resources, `AutopilotGoal` (the control surface: `Some(BlockPos)` starts or retargets, `None` stands down), `AutopilotStatus` (`Idle`/`Planning`/`Driving`/`Failed`/`Arrived`) and a private `AutopilotState`, and two systems chained `.after(TickSet::Intent).before(TickSet::Physics)`: `plan_route` steps a resumable search a bounded number of nodes per tick, and `drive_plan` turns the current edge into `MovementIntent`/`LookIntent` via `WalkDrive`/`ClimbDrive`, closed-loop against the actual `PlayerState`. A debug-only `extract_plan_billboards` draws the plan as a billboard trail.

- **Segmentation.** When the active plan's remaining cost (excluding the executing edge) drops below a lead-tick threshold, a second search dispatches from the terminal node; when edges run out the continuation splices in at edge zero. If none is ready the executor holds still.
- **Witness invalidation.** Each tick a small look-ahead window of upcoming edges is diffed against the live world by block-state id, and a slower full sweep runs on a longer interval. A mismatch, including an unloaded chunk, discards the plan and its continuation and re-searches from the live position. Only state identity is compared; per-edge cost re-verification (a mob in the way) is not modelled.
- **Costing.** Each movement kind is priced by simulating it once against a synthetic collision frame and caching. `WalkDrive::done()`/`arrived()` is a cell-boundary crossing test, so a diagonal measures about 1.17x a straight step from a straight entry and about 0.89x from a reverse entry (not 1.41x), and climbing up (~8.5 ticks/block) is slower than down (~6.67) because gravity is subtracted from the upward target while the downward clamp is a floor on velocity.

## How to change it, and the gotchas

- `WalkDrive` aims at the destination cell centre and varies only by a `jump` flag (`StepUp`/`WalkDiagonal` needed no executor changes). `Climb` needs its own `ClimbDrive` (hold jump to ascend, nothing to descend) and a vertical-only collision frame.
- A move between surfaces of different height needs a same-height check in `arrived()`: the AABB still overlaps the source column for a few ticks, so a horizontal check reports a multi-block drop arrived before it falls. Assume this for any future `MoveKind` with differing heights (or fluid state).
- A climbable block (ladder, vine) has a real non-full shape but does not block motion: treat it as air for support/head-room (never a floor to stand on, in or under) while keeping its shape for physics and its climbable fact for legality; otherwise mounting is refused, bodies stand on ladders, or chains never reach their bottom rung.
- `fall_step` unifies `Descend` and `Drop` (a falling body stops at the first surface; no "land N cells down" variants).
- A legality gate reading the cell *below* the stand position must not run for a body whose feet rest *inside* a partial block (slab, soul sand, snow layer): that block is already the cell being checked.
- `lodestone-autopilot` never touches `ActionQueue`; it only produces intents, and `player_physics` plus `send_move_action` put anything on the wire.
- Keep `plan_route`'s incremental `Search` stepping (a single blocking `Search::run` reintroduces the frame stall); `lodestone_nav::drive::compute_plan` is the sanctioned run-to-completion entry for tests and offline tools.
- A plugin deriving `Resource`/`Component` needs `bevy_ecs`/`bevy_app` as direct dependencies (the derives emit absolute `bevy_ecs::` paths).
- Hermetic tests hand-build a `World` fixture and minimal `VersionAdapter` independent of the physics collision fixture (production reads `ChunkWorld` for planning and `PlayerCollision` for physics through different seams). A flat full-cube fixture cannot exercise partial shapes (slab, ladder); use a jar-derived collision census to prove a shape is handled.

**Not wired into the shipped client**: `lodestone-shell` does not depend on `lodestone-autopilot` (no feature, no chat command). Build your own `lodestone_ecs::app::App` with `AutopilotPlugin` and hand its `World` to `lodestone_client::ClientBuilder::ecs` (as `tests/drives_to_goal.rs` does), or register it via `Sim::client_app()` for a window. Use `lodestone_app::client_app()`'s plugin set: `ControllerPlugin` writes `MovementIntent` one tick-set earlier, so a bot on a smaller ad hoc stack can pass its tests and still lose every tick to the controller in the real set.

## Configuration

`AutopilotGoal` is the only runtime control. `SNAPSHOT_RADIUS` (8 columns, about 143 blocks from the start) is a compile-time constant that also bounds how far a plan gets before continuation. `NavPolicy::default()` governs `max_fall_blocks`, `jump_penalty`, `damage_cost` and `replan_lead_ticks` (30); the plugin exposes none of these and every call site uses the default.

## Dependencies

`lodestone-nav`; `lodestone-ecs` (`ChunkWorld`, `VersionData`, `TickSet`, `MovementIntent`, `LookIntent`, `LocalPlayer`, `GameTick`); `lodestone-model` (`BlockPos`, `VersionAdapter`); `lodestone-physics`; `lodestone-world`; direct `bevy_ecs`/`bevy_app`; dev-only `lodestone-data`. See [plugin API](./plugin-api.md) and [`crates/plugins/README.md`](../crates/plugins/README.md).
