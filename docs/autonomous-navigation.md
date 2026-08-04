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

**Where this stands, honestly:** `lodestone-nav` implements `Walk`, `StepUp`, `Descend`, `Drop` and
`WalkDiagonal` (M2's real-terrain kinds bar climbing — see §"M2, so far" below) plus segmentation
(a journey longer than one snapshot no longer stalls at the boundary). `Climb` is **not**
implemented and is stopped deliberately rather than rushed (see "`Climb`: stopped, and why" below);
breaking and placing are M4/M5. Point the plugin at a reachable block — now including one a block
or two up, down, or off a short drop, or one a 45° corner-cut away — and it walks, jumps or falls
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

### `Climb`: stopped, and why

`docs/baritone-port.md` §9 names `Climb` as the other M2 kind alongside `WalkDiagonal`. It is
**not implemented**, and this was a deliberate stop rather than a rushed third generalisation, made
after `WalkDiagonal` landed and was gated — following exactly the brief this pass was handed
("stopping with a written scope is a good outcome here; a rushed third frame is not").

The reason is structural, not a time-boxing excuse: **`Climb` needs a real second *script*, not
just a second cost-model frame.** Every kind implemented so far — `Walk`, `StepUp`, `Descend`,
`Drop`, `WalkDiagonal` — shares one physical shape: aim at the destination cell's horizontal centre
and either brake or don't (`docs/baritone-port.md` §4.8, and this crate's own "`MoveKind` has five
variants now" entry above records that `WalkDiagonal` needed zero changes to `drive::edge_drive`
because of it). Climbing a ladder or a vine is not that shape at all — `docs/baritone-port.md` §2.3's
own catalogue says as much: "pressing forward while airborne beside a climbable block makes you
grab and climb it instead of moving forward", and holding a direction key against a climbable
column is the entire mechanism, with no horizontal aiming involved once mounted. `WalkDrive`'s
`target()`/`inside_cell()`/`arrived()` are all expressed in terms of a horizontal destination cell
and a single surface height; a ladder's own "are we done" question is about a *column* and a
*vertical* position, which is a different completion test, not a parameter to the existing one.

Concretely, `Climb` would need at minimum:

- **A second `DriveTick` producer** (`docs/autonomous-navigation.md`'s own "`MoveKind` has five
  variants now" entry already flagged this as the next thing that should grow `edge_drive`'s
  `match` for real) — one that holds a direction key rather than solving `(forward, strafe)` from a
  world-space direction, and mounts/dismounts a `Climb` edge as two phases the way `Break`/`Place`
  are already documented as two-phase in `docs/baritone-port.md` §4.8.
- **A vertical cost-model frame.** `TemplateTable::simulate`'s two existing frames are `+x`
  (cardinal) and `+x, -z` (diagonal, this pass); a ladder's own frame is `+y` (or `-y` descending,
  capped at a different rate per `docs/baritone-port.md` §4.3's own worked table: `0.2` b/t up,
  `0.15` b/t down). Nothing here suggests that frame is hard to build — the stencil-world and
  entry-state machinery both generalise the same way `WalkDiagonal`'s did — but it is real,
  additional, untested work, not a parameter to the frame this pass already built.
- **A real legality predicate** over `BlockFacts::climbable`, including mounting (approaching a
  climbable column while grounded, per the airborne-grab trap above) and dismounting (stepping off
  the top or bottom onto ordinary ground).

None of this is started. `MoveKind` has no `Climb` variant, `BlockFacts::climbable` is read by
nothing in this crate yet, and no stencil, legality rule or template key exists for it. This is
recorded here rather than left to be rediscovered, exactly as `WalkDiagonal`'s own former "not
implemented" note (removed from `## What it is` above now that it is done) was recorded by the
predecessor who stopped short of it.

## How to change it, and the gotchas

- **`MoveKind` has five variants now — `Walk`, `StepUp`, `Descend`, `Drop(Dir4, n)`,
  `WalkDiagonal(Dir4, Dir4)` — and the M1 forcing function already did its job twice.**
  `drive::edge_drive` used to have `let lodestone_nav::MoveKind::Walk(_) = edge.kind;`, an
  irrefutable-pattern assertion that stopped compiling the moment a second kind landed, forcing a
  real `match` rather than a silent mis-handle. The answer for **all four** M2 additions turned out
  to be "no new script needed" — `WalkDrive` already aims at the destination cell centre (both `x`
  *and* `z`, unconditionally) and brakes-or-doesn't identically regardless of how many axes moved;
  the only physical difference across all five variants is `WalkDrive::jump`, a plain bool set for
  `StepUp` only. `edge_drive` needed **zero** changes to add `WalkDiagonal` — the entire executor
  layer generalised for free. `Climb` is the one that will not: see "`Climb`: stopped, and why"
  below.
- **`WalkDiagonal` generalised cleanly in three places and needed real new work in two — knowing
  which was which is the actual deliverable, not just the code.** Clean generalisations, each
  reusing an existing mechanism verbatim: the **executor** (`drive.rs`, above); the **legality
  stencil pattern** (`graph::diagonal_step` reuses `walk_step`'s own hazard/head-room checks for its
  two shoulders, and `diagonal_stencil` is `column_stencil`'s four-column generalisation of the same
  idea); and the **heuristic** (`goal::octile`'s doc comment already said it would be exact once
  this landed, and it is — see below). Two things needed a genuine second frame:
  - **The cost model's canonical simulation frame.** `cost::TemplateTable::simulate` used to place a
    kind's destination at `[1, 1 + rise, 0]` — a pure `+x` canonical frame every cardinal kind
    shares. `WalkDiagonal` moves along `+x` **and** `-z` at once (the canonical pair is always
    `(North, East)`; every real diagonal is rotated onto it, exactly as every real cardinal direction
    already rotates onto `+x`), so the destination is `[1, 1, -1]` and the entry-state formulas
    needed a genuine two-axis treatment — see `EntryRel::of_diagonal`'s own doc comment for why that
    turned out to need only **three** entry classes (`Still`/`Straight`/`Reverse`, reusing the
    cardinal position formulas verbatim) rather than five: a cardinal arrival is always exactly `45°`
    or `135°` off a diagonal's own heading, never `0°`, `90°` or `180°`, and the diagonal's own mirror
    symmetry makes the two `45°` members (and the two `135°` members) of each pair cost-equivalent.
  - **The sub-tick completion fraction — and this one was a real, found bug, the diagonal analogue of
    the `arrived()` straddle the predecessor found for `Descend`/`Drop`.** `simulate`'s "the boundary
    was crossed partway through this tick, don't charge a whole tick for it" refinement was
    hardcoded to `x` alone, because every cardinal kind only ever moves along `x`. `WalkDrive::done`
    requires **both** `x` and `z` inside the destination cell, and a diagonal's two axes can (and
    typically do) cross their own boundaries on *different* ticks — so on the tick `done()` first
    fires, whichever axis crossed earlier is no longer moving toward anything meaningful, and
    measuring "how far into this tick" against a boundary crossed ticks ago produces a number with no
    physical meaning. Fixed by `cost::completion_fraction`, which only credits an axis that is
    **newly** inside its target cell this tick, and — when both are newly inside on the same tick —
    takes the **later** (larger) of the two, since completion needs both. Provably backward
    compatible with every cardinal kind (`z`'s target always equals its start, so it is never "newly
    inside" and never contributes — see the function's own doc comment and
    `completion_fraction_matches_the_original_single_axis_formula_when_z_never_moves`), so the fix
    only ever changes `WalkDiagonal`'s own numbers.
  - **A real, load-bearing side effect of the frame change:** `cost::TemplateTable::cheapest_ticks_per_block`
    used to scan only steady-state *cardinal* speed on the reasoning that nothing moves faster per
    block than continuing in a straight line. That reasoning broke the moment `WalkDiagonal` existed:
    its `Reverse` entry class measured **~3.09 ticks per octile block** against a cardinal-only
    heuristic rate of **~3.46** — the heuristic *overestimating* true cost for a diagonal approached
    that way, a genuine admissibility violation this crate's own `debug_assert`-backed contract
    exists to forbid. Not because a diagonal is actually faster than steady state — because
    `Reverse`'s aligned axis inherits almost no residual distance from a prior cardinal edge's own
    **boundary**-crossing completion (`WalkDrive::done` is a cell-boundary test, not a
    "reached-centre" test). Fixed by also scanning every diagonal template `EntryRel::of_diagonal`
    can actually produce into the same minimum — see `cheapest_ticks_per_block`'s own doc comment and
    `the_heuristic_rate_still_bounds_every_diagonal_entry_classs_own_rate`.
  - **One measured number that does not match the design doc's own estimate, recorded rather than
    quietly reconciled:** `docs/baritone-port.md` §4.1 says a diagonal should cost "a hair below
    `sqrt(2)` times a straight step". Measured here (`Straight` entry, open flat ground): **~1.17×**,
    not `~1.41×` — and `Reverse` entry measures **~0.89×**, genuinely *cheaper* than one cardinal
    step. The reason is the same boundary-vs-centre distinction above: the design doc's figure
    describes a full centre-to-centre Euclidean crossing, and `WalkDrive::done` measures a
    cell-boundary crossing on both axes — a different, smaller distance for exactly the entry classes
    that inherit a "just crossed a boundary" position from a prior cardinal edge. What actually
    matters for the search (a diagonal must beat a two-edge cardinal detour of the same net
    displacement) still holds — see `a_diagonal_step_costs_less_than_two_cardinal_steps` — so this is
    a genuine finding about *why* the number differs, not a bug needing a fix.
  - **The corner-cutting rule is real vanilla source, cited, not intuition.** A diagonal can be
    physically blocked even with an open destination cell — the player's `0.6`-wide body clips a
    solid corner unless both orthogonal neighbours ("shoulders") are clear. The discrete rule comes
    from the *mob* pathfinder's own diagonal check —
    `WalkNodeEvaluator.isDiagonalValid(pos, ew, ns)`
    (`.cache/mc/26.2/src/net/minecraft/world/level/pathfinder/WalkNodeEvaluator.java:167-182`):
    both shoulders must be legally walkable *and* neither may sit above the current cell
    (`ns.y > pos.y || ew.y > pos.y` refuses outright, before cost is ever considered) — citing this
    is deriving a real Minecraft fact about the moving body's shape from real source, not extending
    the mob pathfinder itself (`docs/baritone-port.md` §3.4's "do not extend it" is about the
    pathfinder's *search*, not about facts a player-navigator can independently derive from the same
    source). `graph::diagonal_step` reuses `walk_step` for the "legally walkable" half and adds the
    `y <= from.y` gate on top — including the case a plain `walk_step` reuse would miss: a shoulder
    that is itself a perfectly legal one-cell-**up** `Walk` (stepping off soul sand onto stone beside
    it) is still refused as a diagonal shoulder, exactly as vanilla refuses it
    (`a_shoulder_that_is_a_legal_walk_but_one_cell_higher_still_refuses_the_diagonal`). One real
    vanilla permissiveness is deliberately **not** replicated — accepting a strictly-lower shoulder
    regardless of hazard — because it does not fit this crate's own conservative hazard policy; see
    `diagonal_step`'s own doc comment.
  - **`WalkDiagonal`'s exit `Arrival` collapses onto its first component (`Arrival::Walking(d1)`)
    rather than gaining a genuinely diagonal variant, and this is a real, bounded approximation, not
    an oversight.** `NavNode::try_pack`'s 64-bit key spends exactly 3 bits (0..=7) on
    `Arrival::index()`, of which only 3 (`5, 6, 7`) are free — one short of the 4 a full diagonal
    arrival set needs — and the other 61 bits are already exactly spent covering the real world
    border (`±29,999,984`), so widening the field is not a free edit. The cost is a slightly
    approximate `turn_penalty` charge on whichever edge follows a diagonal (see
    `EntryRel::of_diagonal`'s own doc comment) — a *preference*, not a measurement, and therefore a
    cheap place to absorb the approximation.
  - **`WalkDrive::arrived()`/`done()` needed no change for a diagonal approach, and this is verified,
    not assumed.** Unlike the cost model's sub-tick fraction, `WalkDrive::inside_cell` was already a
    two-axis test (`floor(x) == cell[0] && floor(z) == cell[2]`), and `WalkDiagonal` never changes
    surface height (same-cell-height family, like `Walk`), so the straddle trap `StepUp`/`Descend`/
    `Drop` needed `arrived`'s own surface-height check for does not apply — a diagonal never
    straddles two *different* surfaces, only two different horizontal cells at once.
    `the_planned_cost_matches_what_executing_a_diagonal_plan_costs` replays a real diagonal plan
    through the real integrator end to end and confirms both axes actually arrive, not just one.
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
