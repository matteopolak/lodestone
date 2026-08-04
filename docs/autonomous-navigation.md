# Autonomous navigation: `lodestone-nav` + `lodestone-autopilot`

## What it is

Two crates under [`crates/plugins/`](../crates/plugins/) implementing M1 and part of M2 of
[`docs/baritone-port.md`](./baritone-port.md)'s Baritone-class navigation design:

- [`lodestone-nav`](../crates/plugins/lodestone-nav) — the version-free search core. A plain library:
  no bevy, no ECS, no threads. `(snapshot, start, goal, policy, budget) → plan`, plus `WalkDrive` and
  `ClimbDrive`, "given a plan edge and a `PlayerState`, what keys do I press this tick".
- [`lodestone-autopilot`](../crates/plugins/lodestone-autopilot) — the bevy plugin wrapping it: a
  goal resource in, `MovementIntent`/`LookIntent` components out, through the exact same seam
  documented in [`docs/plugin-api.md`](./plugin-api.md).

**Where this stands, honestly:** `lodestone-nav` implements `Walk`, `StepUp`, `Descend`, `Drop`,
`WalkDiagonal` and now `Climb` — all of M2's real-terrain kinds — plus segmentation (a journey
longer than one snapshot no longer stalls at the boundary). Breaking and placing are M4/M5. Point
the plugin at a reachable block — now including one a block or two up, down, or off a short drop, up
or down a ladder or a vine, or one a 45° corner-cut away — and it walks, jumps, falls or climbs
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

**Gate**: `tests/drives_to_goal.rs`'s `a_goal_beyond_the_first_snapshot_is_reached_by_splicing_a_continuation`
sends the goal to `x = 200` while a single search's view caps out at `x = 143` — `Arrived` is only
reachable through this test if a second search actually ran and its plan was actually spliced on.

### Witness-set invalidation and the look-ahead window: a committed plan re-checks itself

`docs/baritone-port.md` §4.5. Segmentation above made a plan longer than one snapshot possible to
*drive*; it said nothing about the terrain that plan's legality depended on staying what it was. A
spliced plan that walks blindly into a block someone broke or placed after the search ran is the
failure mode CLAUDE.md's brief for this work calls out as "the one that bites a real player" — and
until this pass, nothing in `plan_route` ever looked back at the world once a plan was adopted.

**The mechanism, in `lodestone_nav::witness` (`crates/plugins/lodestone-nav/src/witness.rs`) plus
`lodestone_autopilot::plan_route`'s invalidation block:**

1. The instant a plan is adopted — a fresh search's `Reached`/`BudgetExhausted`/`WorldExhausted`
   result, or a continuation splicing in — `plan_route`'s `sample_witness_baseline` (or, for a
   splice, `drive_plan` moving `continuation_witness_baseline` over) snapshots every witnessed
   cell's **block-state id** via `lodestone_nav::witness::sample`, keyed by `Plan::witnesses()`'s
   packed `NavNode` keys. This is `AutopilotState::witness_baseline`.
2. Every tick a plan is active, before anything else runs: a cheap look-ahead check
   (`Plan::witnesses_in_range(state.edge..state.edge + LOOKAHEAD_EDGES)`, `LOOKAHEAD_EDGES = 3`)
   diffs that narrow window's cells against the live `ChunkWorld` via
   `lodestone_nav::witness::point_state`. This is §4.5/§2.3's "verify a small window of upcoming
   edges... so a hazard is detected before you are standing next to it."
3. If the window finds nothing, a rate-limited (`WITNESS_SWEEP_INTERVAL_TICKS = 20`) full sweep of
   the plan's *remaining* witnesses (`state.edge..plan.len()`) runs via `witness::first_change` —
   catching a change further down the route the window has not reached yet (a player breaking a
   block fifty edges ahead).
4. Either hit sets `need_fresh`, exactly like a goal change: `plan_route` discards the plan, the
   continuation and both baselines, and dispatches a brand-new search from the player's *live*
   position — §4.9's "never execute a plan you already know is stale."

**Why this samples rather than subscribes, and what that costs.** §4.5's own design assumes a
`SectionBlocksChanged`/`BlockChangedAck` event stream a witness set is tested against on arrival,
`O(block updates)` per tick. **`lodestone-ecs` emits no such event to a plugin today**, and adding
one is outside this crate's ownership — extending the ECS event surface is a different crate's
call to make. So this samples the live world directly and diffs against the baseline instead:
`O(cells checked)` per check, the same asymptotic shape as the design's own analysis, paid on a
caller-chosen cadence rather than event-driven. The two-tier cadence above is exactly what keeps
that bounded: the window is a handful of cells every tick, and the full sweep is bounded by one
segment's own snapshot footprint (never the whole journey — see "prefix trimming" below) and runs
at most once every twenty ticks.

**Only raw block-state ids are compared, never re-derived legality.** A hit means "this witnessed
cell no longer reads what it did at commit time," not "and here is why it is now illegal." That
matches §4.5's own letter ("a hit marks the plan stale") rather than its event source, needs no
`FactsTable`/`VersionAdapter` at verification time at all, and folds the "chunk unloaded under the
plan" case into the same trigger — `witness::point_state` returning `None` where it used to return
`Some` counts as a change too, conservative by construction.

**Gate, and its own control, first** (`tests/drives_to_goal.rs`):
`a_block_broken_under_a_committed_plan_forces_a_replan_around_it` walks a flat corridor, breaks the
support block the committed plan's own witnesses cover a few blocks ahead, and asserts the executed
path actually diverges from the original straight line while crossing that column — the decisive
signal, because this fixture's `FlatFloor` physics seam is independent of `ChunkWorld` (production's
two seams, `ChunkWorld` for planning and `PlayerCollision` for physics, kept deliberately separate),
so nothing about the *executor* would ever notice the missing block; only the planner re-checking
its own witnesses can produce a different route. **Watched to fail**, not merely asserted to:
short-circuiting `invalidated_at` to `None` (recorded, then reverted via the `cp`+`md5` discipline
CLAUDE.md's neuter-window rule asks for) reproduces the exact failure the fix exists to prevent —
the executed path stays the stale straight line and the assertion trips. A more direct assertion —
sample `AutopilotStatus::Planning` after the break — does not work here and is worth recording as a
trap: on this trivial, open-flat-ground search, `plan_route`'s `Planning` write and the same-tick
`search.step`-to-`Reached` overwrite happen inside one system call, so a status sampled once per
tick (as every other test in this file does) can never observe it, true even for the very first
plan's adoption, before any invalidation exists to blame.

**What is still deliberately not built: per-edge cost re-verification (the *inflated* half) and
early adoption.** §4.5 names per-edge cost re-verification as "a cell can be unchanged and the edge
still more expensive (a mob in the way, a fluid level shift)" — its *impossible* outcome is already
subsumed here (a cell becoming illegal is exactly a witnessed-cell change, already caught above),
but this crate has no mob-avoidance and only an approximate, non-per-tick fluid model (`view.rs`'s
own doc comment on `fluid_at`), so there is no currently-modelled *live* cost driver beyond what the
witness diff already catches — a genuine re-verification of the *inflated* case has nothing further
to check against today, not a gap silently left open. §4.9's early adoption ("if an edge has just
cleanly completed and your position appears anywhere in the pending continuation, hop straight onto
it") is unrelated to invalidation and still genuinely unbuilt.

**Prefix trimming is not needed here, and this is a finding, not a gap.** §4.9 motivates it with "on
a long journey the plan grows without bound and the per-tick scans over it dominate" — true of a
design that *concatenates* segments into one ever-growing `Plan`. This implementation never does
that: `drive_plan`'s splice (`state.plan = Some(next)`) **replaces** the active plan wholesale with
the continuation's own, independently-bounded `Plan` object; it never appends. So a single `Plan`'s
length — and therefore every `O(plan)` operation on it, including this section's own full-sweep
witness check — is bounded by one segment's snapshot radius for the entire journey, never by how far
the player has travelled in total. Nothing to trim, because nothing grows.

### `sim.rs` registration and `#goto`: both closed, re-verified rather than assumed

Issue #38's title ("shell built and driving in a hermetic test; needs `sim.rs` registration +
`#goto` command") is stale — both halves are done and were re-verified against the tree directly
for this pass, not from the plan or from this document's own prior wording:

- `lodestone_shell::sim::Sim::new`'s `app.add_plugins((CorePlugin, LocalPlayerPlugin,
  ControllerPlugin, …, InteractPlugin, lodestone_autopilot::AutopilotPlugin))` tuple has
  `AutopilotPlugin` in it (`crates/lodestone-shell/src/sim.rs`), plus a
  `lodestone-autopilot = { workspace = true }` line in `crates/lodestone-shell/Cargo.toml`'s
  `[dependencies]`. `sim::tests::autopilot_plugin_is_registered_and_its_systems_actually_run` (in
  `crates/lodestone-shell/src/sim/tests.rs`) proves the two systems actually *run*, not merely that
  the plugin is in the list.
- `#goto x z` is a real, tested client-local chat command: `Sim::send_chat` intercepts any
  `#`-prefixed line before `compose_chat_action` ever sees it (`sim.rs`'s own doc comment: "any
  `#`-prefixed line is consumed here, matched or not"), `parse_goto_command` parses it, and a
  well-formed one writes `AutopilotGoal` directly. `sim/tests.rs`'s
  `goto_chat_command_drives_the_player_toward_the_goal_over_real_ticks` ticks a real schedule and
  asserts `AutopilotStatus::Arrived`; `goto_chat_command_never_reaches_the_outbound_action_queue`
  is the negative control that `#goto` never leaks onto the wire as ordinary chat, and that a
  malformed one is not silently swallowed.

Both landed in `bc41685` and `2830ea2`, some time before this pass. Issue #38 remains open on the
tracker; nothing in this crate's remaining scope depends on it, and it should be closed with this
finding rather than left to mislead the next reader into re-doing already-done work.

### `Climb`: landed, and what the two hard parts actually needed

`docs/baritone-port.md` §9 names `Climb` as the other M2 kind alongside `WalkDiagonal`. Two
predecessors stopped short of it and recorded exactly two hard parts up front: a genuinely different
input script (holding a direction key against a climbable column, not aiming at a cell centre) and a
third cost-model frame (vertical). Both stops were correct — neither part turned out to be small —
and this section replaces the former "stopped, and why" note with what each one actually needed.

**Check-before-building paid off once, immediately.** `lodestone-physics` already fully models
climbable ascent: `LivingEntity.handleOnClimbable`'s velocity clamp and `travel_in_air`'s
"steady climb-up" override (`entity.rs`) are both there, unconditional on the block at the feet
position — nothing about them is specific to a deliberate climb versus, say, brushing past a ladder.
So this pass's job really was legality, cost and drive, never a physics change, exactly as the brief
predicted.

**The drive: `ClimbDrive`, and why jump beats forward.** Ascending holds jump every tick, never
forward/strafe — `travel_in_air`'s override fires on `ctx.jumping` alone, with no collision required,
which is the one script that works for both a ladder (which has a wall to press into) and a
free-hanging vine strand (which may not). Descending holds nothing at all: `handle_on_climbable`'s
own velocity floor (`-0.15`) already caps the fall. `ClimbDrive::done` is deliberately **not**
`WalkDrive::done` with a parameter — the brief's own framing ("a climb is entirely vertical, so
'arrived' cannot mean an in-cell horizontal test at all") is exactly right: it is a vertical
cell-boundary crossing, gated on `on_ground` only when the destination is real ground, never when it
is another climbable cell mid-column (a clinging body is never grounded, and requiring it there would
hang the executor forever).

**The frame: a real, separate `ClimbStencilWorld`, and a real, separate `simulate_climb` — not a
`rise` parameter.** `TemplateTable::simulate`'s existing frame is "a floor that steps once in `x`";
climbing has no floor at all (the body clings, never stands) and no horizontal displacement, so
neither the existing stencil world nor `WalkDrive` applies. The vertical frame needed its own
`CollisionView` (climbable everywhere in one column, solid nowhere) and its own drive, confirming the
predecessors' concern that this was real, additional work, not a small edit.

**Whether the vertical frame admits `WalkDiagonal`'s entry-class collapse: yes for costing, no for
node identity — and those are genuinely different questions.** For *costing*, `Climb` needs no entry
classification at all, not even the diagonal's three-way one: the script presses no forward/strafe,
so there is no horizontal momentum for any `EntryRel` variant to describe, and every call site fixes
`EntryRel::Still` unconditionally (`search::Search::expand`'s own comment). That is a *stronger*
collapse than the diagonal's. But `Arrival` — the node's own identity, not a costing input — needed a
sixth variant, `Climbing`, for a reason that has nothing to do with cost: the executor's `ClimbDrive`
must know whether an edge's destination is a dismount onto real ground (requires `on_ground`) or
another climbable cell mid-column (never grounded), and `to_surface` cannot tell the two apart — a
full-block dismount's surface is numerically identical to a continuing climb's nominal cell-floor
reference. `Arrival::Climbing` carries no direction (climbing has no horizontal heading), so it still
costs a following edge identically to `Still` — the collapse holds exactly where it mattered for
costing, and the one place it does not hold is a fact about the *executor*, not about ticks.

**Two real, previously-latent bugs found in `graph::stand_surface`/`head_room`, both invisible until
a climbable cell existed in any fixture.** A ladder's real collision shape is full-height (`top ==
1.0`, thin only against the wall — `Block.boxZ(16.0, 13.0, 16.0)`, cross-checked against
`lodestone_model::adapter::block_blocks_motion`'s own `0.7291666666666666` mean-extent figure) but
`blocks_motion == false` (`forceSolidOff`). Two functions that had never had to reconcile "real shape"
with "does not actually support anything" both got it wrong:

- `stand_surface`'s "inside" branch read `top == 1.0` as "filled, refuse" — the same refusal a solid
  wall gets — so standing *in* a ladder's own cell, i.e. mounting it at all, was refused outright.
- `stand_surface`'s "below" branch read a climbable one cell under a candidate stand cell as a full
  support, letting a body appear to stand *on top of* a ladder or vine from above, which real
  collision never permits.
- `head_room`'s sweep read the ladder's nonzero shape as `!passable` for every cell within a body's
  height of one — including the climbable's own stand cell checking the rung directly above it, so a
  `Climb` chain could not even mount the bottom rung.

All three are fixed by treating a climbable cell as `AIR` for support/headroom purposes specifically
— never a floor to stand on, in, or under, always something to look past — while leaving its
`climbable` fact and its real (thin) shape intact for physics. None of `lodestone-nav`'s 75
pre-`Climb` unit tests exercises this path (no fixture had a nonzero-shape, non-full-cube,
non-blocking block before), which is the same "world" species of vacuous test the real-collision
gates below exist to close.

**One measured number that contradicts `docs/baritone-port.md` §4.3's own worked table, recorded
rather than reconciled.** That table gives climb-up `0.2` b/t (5.0 ticks/block) and climb-down
`0.15` b/t (6.67 ticks/block) — i.e. up faster than down. Simulating the real integrator gives the
**opposite ordering**: steady climb-**up** is `(0.2 − gravity) × vertical_air_drag = (0.2 − 0.08) ×
0.98 = 0.1176` b/t (~8.5 ticks/block), and climb-**down** is exactly `handle_on_climbable`'s own
`0.15` floor (~6.67 ticks/block, matching the table). The reason is mechanical, not a modelling
choice: `travel_in_air`'s climb override sets a **raw**, pre-gravity `0.2` target every tick, and
real vanilla's own gravity subtraction reduces it *before* it ever reaches `move_entity` — the
design doc's `0.2` describes the override's input, not its simulated output. Down has no
symmetric reduction because `handle_on_climbable`'s clamp is a floor applied directly to the
pre-move velocity, not a target subject to a further subtraction. `cost::tests::climb_up_steady_rate_matches_the_gravity_and_drag_derived_formula`
and its `climb_down_...` counterpart pin both numbers against those cited constants directly, and
`climbing_down_costs_fewer_ticks_than_climbing_up` records the resulting, real, surprising ordering
— the same kind of finding `WalkDiagonal`'s own `1.17×`/`0.89×` (against a design estimate of "a hair
below `sqrt(2)`") already established for this crate: simulate, then report what the design doc got
wrong, never silently "correct" the measurement to match the doc.

**A real entry-position bug in the vertical frame's own simulation, found by the admissibility check
going strongly negative — the same failure class `WalkDiagonal`'s `cheapest_ticks_per_block` gap
was.** The first version of `simulate_climb` seeded every direction at the source cell's exact
integer floor (`y = 1.0`). For `Up` that is correct (a genuine `1.0`-block climb to `y = 2.0`); for
`Down` it is a near-zero-distance start, because `floor(1.0) == 1` already, so any downward drift at
all immediately satisfied "reached cell `0`" — measured **one tick per block**, which briefly made
climbing the fastest movement in the entire template table and collapsed `cheapest_ticks_per_block`'s
heuristic to the bare deflation constant. Fixed by seeding each direction "just crossed into the
source cell in the direction of travel" (`Up` at `1.001`, `Down` at `1.999`) — the same convention
`entry_state`'s `Straight` already uses on the horizontal axis, applied to `y`. The one case this
does not model precisely — a body that freshly mounted the *top* of a ladder and immediately
descends genuinely starts nearer the boundary than this seeding assumes — is a documented, bounded,
safe-direction exception: it makes that one edge's template an overestimate, never an underestimate,
the same shape as the ground-jump-hop exception below.

**A second, bounded, safe-direction exception: the simulation never seeds `on_ground = true`.** Real
mounting sometimes begins on solid ground (walking into a ladder's own footprint at floor level), in
which case vanilla's ordinary ground-jump impulse (`0.42`) fires on the very first tick alongside the
climb override. Modelling that exactly would need a second climb template (mount-from-ground vs.
continue-while-clinging) for a one-tick effect; instead every template seeds `on_ground = false`
throughout, which is correct for every edge but the very first `Climb(Up)` in a chain and, for that
one, makes the real executor finish a hair faster than the template predicts — never slower. Recorded
in `TemplateTable::simulate_climb`'s own doc comment rather than hidden.

**One change `Climb` forced onto a kind that predates it: `graph::fall_step` now refuses a direction
whose landing scan passes through a climbable cell.** Real `travel_in_air` arrests a fall the instant
the feet cross into a climbable cell, unconditionally — not only while deliberately climbing — so
`Descend`/`Drop`'s plain-gravity `StencilWorld` would silently disagree with real physics for any
column containing one. `graph::tests::a_fall_through_a_climbable_column_is_refused` is the gate.

**Real-collision gates, both directions.** `lodestone-autopilot/tests/drives_to_goal.rs`'s
`real_collision` module gained `a_real_ladder_is_climbed_from_the_ground_to_a_real_platform` (a real
`minecraft:ladder` state, through the real `VersionAdapter → AdapterCensus → FactsTable` chain,
predicting the exact plan — `Walk, Climb(Up), Climb(Up), Walk` — through `compute_plan`) and its
unreachable control, `a_real_ladder_with_no_platform_at_the_top_cannot_reach_a_goal_there` (the same
ladder with nothing to dismount onto; `compute_plan` returns `None`, watched to fail rather than
merely asserted). `search::tests::the_planned_cost_matches_what_executing_a_climb_plan_costs`
replays a full mount-climb-dismount plan through the real integrator end to end, the strongest gate
available here, mirroring `WalkDiagonal`'s own precedent.

**`Drop` had no real-collision gate of its own, and now does.** The slab gate above strengthens
M1's `Walk`; `StepUp`/`WalkDiagonal`/`Climb` each got the same treatment on landing. `fall_step`'s
`n > 1` branch — the slab-exclusion rule, the hazard-in-the-fall-path scan, the whole "a falling body
stops at the first surface it reaches" legality — never had a real-jar counterpart, only
`FixtureCensus`-level unit tests. Closed three ways: `drives_to_goal.rs`'s
`real_collision::a_goal_past_a_real_two_cell_drop_is_reached_by_falling` (a real `minecraft:stone`
two-cell cliff, through the real `TickSet::Intent -> Physics` pipeline) and its unreachable control
`a_real_drop_deeper_than_max_fall_blocks_cannot_reach_a_goal_past_it` (the same shape at a four-cell
depth, past `NavPolicy::default().max_fall_blocks`, `compute_plan` returns `None`, watched to fail);
and, in `lodestone-nav` itself,
`search::tests::the_planned_cost_matches_what_executing_a_drop_plan_costs` — the same "planned cost
equals executed cost" replay `Walk`/`WalkDiagonal`/`Climb` already had, which `Drop` had not, so
nothing before this ever confirmed `WalkDrive::arrived`'s same-height straddle fix (the "a synthetic
`Drop` of 2 cells and one of 6 both completed in identically 4.93 ticks" bug, described below) holds
once physics, not a hand-picked `to_surface`, decides when the edge is actually done — plus its own
search-level unreachable control, `a_drop_deeper_than_max_fall_blocks_has_no_route_across_it`.

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
  layer generalised for free. `Climb` is the one that did not: see "`Climb`: landed, and what the
  two hard parts actually needed" above — `crate::drive::edge_drive` (in `lodestone-autopilot`) now
  returns an `EdgeDrive` enum (`Walk(WalkDrive)` / `Climb(ClimbDrive)`) rather than a bare
  `WalkDrive`, and `MoveKind` itself has seven variants (`Climb(ClimbDir)`, `ClimbDir` being `Up`/
  `Down`, folded into two separate `id()`s rather than one, because — unlike a cardinal `Dir4` —
  the climb direction is genuinely cost-relevant).
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
