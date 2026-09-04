//! The bevy plugin wrapping [`lodestone_nav`]: worker orchestration, the
//! per-tick closed-loop executor, and (future milestones) chat commands and a
//! debug overlay.
//!
//! Designed against [`docs/baritone-port.md`](../../../docs/baritone-port.md);
//! see [`docs/plugin-api.md`](../../../docs/plugin-api.md) for the surface this
//! crate consumes and nothing more — it is an ordinary third-party plugin, not
//! engine code, and every seam it uses (`TickSet::Intent`, `MovementIntent`,
//! `LookIntent`, `ActionQueue`... this crate does not even need `ActionQueue`,
//! see below, `ChunkWorld`, `VersionData`) is public API a *different* plugin
//! could use too.
//!
//! # What M1 is, honestly
//!
//! `lodestone-nav` is M1: `Walk` only. This plugin is therefore also M1: point
//! it at a reachable block on flat-ish ground (no jumps, no breaking, no
//! placing — those are M2+ in the search core, and this plugin gains nothing
//! by getting ahead of it) and it walks there, closed-loop, through the real
//! `TickSet::Physics` integrator. That is deliberately not a finished bot; it
//! is the seam proven end to end, which is the thing that did not exist before
//! this crate — see this crate's own tests for the proof, and
//! `docs/autonomous-navigation.md` for what is still missing.
//!
//! # Why this system does not need `ActionQueue`
//!
//! `docs/plugin-api.md` documents `ActionQueue` as the sanctioned way a plugin
//! reaches the wire. This plugin never touches it, and that is not an
//! oversight: [`MovementIntent`] and [`LookIntent`] are consumed by
//! [`lodestone_ecs::player::player_physics`] (`TickSet::Physics`), whose output
//! *is* what `lodestone_controller::ecs::send_move_action` (`TickSet::Send`)
//! reports on the wire every tick regardless of who drove the intent. Writing
//! `ActionQueue` directly would be a second, competing route to the same
//! packet and is exactly the kind of thing `docs/plugin-api.md`'s "what stays
//! privileged" section does not need to forbid, because the intent seam already
//! makes it redundant.
//!
//! # Ordering: after the whole of `TickSet::Intent`, not inside it
//!
//! `crate::drive_plan` is `.after(TickSet::Intent).before(TickSet::Physics)` —
//! the exact idiom `lodestone_ecs::sets::TickSet::Intent`'s own doc comment
//! names for "override human input this tick". Ordering strictly after the
//! whole set (rather than `.in_set(TickSet::Intent)` alongside
//! `compute_movement_intent`) means this plugin never has to name that system
//! function — anchors are sets, not functions, precisely so a plugin does not
//! have to (`docs/plugin-api.md`'s "how to change it") — and there is no
//! ambiguity for `ambiguity_detection: LogLevel::Error` to catch, because the
//! order is explicit rather than inferred.

use std::collections::HashMap;
use std::sync::Arc;

use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::ecs::prelude::{Commands, Entity, Query, Res, ResMut, With};
use lodestone_ecs::ecs::resource::Resource;
use lodestone_ecs::ecs::schedule::IntoScheduleConfigs;
use lodestone_ecs::player::PhysicsState;
use lodestone_ecs::{
    ChunkWorld, Extract, ExtractSet, GameTick, LocalPlayer, LookIntent, MovementIntent,
    PluginBillboard, PluginBillboards, PluginTexture, TickSet, VersionData,
};
use lodestone_model::BlockPos;
use lodestone_nav::{
    AdapterCensus, Budget, FactsTable, NavPolicy, Outcome, Plan, Progress, Search, witness,
};
use lodestone_physics::{MovementInput, Vec3d};

pub mod drive;

pub use drive::compute_plan;

/// How many columns in every horizontal direction a search snapshot spans
/// (`docs/baritone-port.md` §4.2 defaults this to 12 for a full 25×25; M1's
/// short-range demo use case does not need that much, and a smaller snapshot
/// is a smaller [`SnapshotView::build`] copy every time the goal changes).
pub const SNAPSHOT_RADIUS: i32 = 8;

/// How many upcoming edges the look-ahead check inspects, every tick, while a
/// plan is being driven (`docs/baritone-port.md` §4.5/§2.3's "verify a small
/// window of upcoming edges... so a hazard is detected before you are
/// standing next to it"). Checked unconditionally rather than only "when a
/// new edge starts": the check itself only ever samples a handful of cells
/// (this many edges' stencils, a few dozen cells at most —
/// `MoveKind::stencil`'s own doc comment bounds each at "≤ ~20"), so running
/// it every tick is cheap enough that tracking "did the edge index just
/// change" would add complexity without buying anything.
const LOOKAHEAD_EDGES: usize = 3;

/// Ticks between full re-samples of the *rest* of the active plan's witness
/// set — `docs/baritone-port.md` §4.5's witness-set invalidation proper, for
/// the part of the route beyond the look-ahead window (a player breaking a
/// block fifty edges ahead, say). Rate-limited because, unlike the window
/// check, this one is `O(remaining plan)`; bounded by one segment's own
/// snapshot radius (segmentation never lets a single [`Plan`] grow past
/// that — see this crate's docs on why prefix trimming is not needed here),
/// but still not something to pay every tick.
const WITNESS_SWEEP_INTERVAL_TICKS: u32 = 20;

/// The plugin's public control surface: set a goal to start walking there, or
/// `None` to stand down and hand control back to whatever last held
/// [`MovementIntent`]/[`LookIntent`] (human input, by default — see the crate
/// docs on ordering).
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AutopilotGoal(pub Option<BlockPos>);

/// Where the plugin is with the current [`AutopilotGoal`] — read-only status
/// for a caller (a chat command handler, a debug overlay, a test) that wants
/// to know more than "is it moving".
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AutopilotStatus {
    /// No goal set, or the goal was just cleared.
    #[default]
    Idle,
    /// A search is in progress (`lodestone_nav::Search::step`, spread over
    /// ticks — see [`plan_route`]'s doc comment on why this must not block a
    /// frame).
    Planning,
    /// A plan exists and is being executed.
    Driving,
    /// The most recent search finished without reaching the goal. The goal
    /// resource is left as the caller set it — this plugin does not clear it
    /// on failure, matching `docs/baritone-port.md` §2.3's "abandonment must
    /// not be reported as failure" for a *behaviour* layer above this one; at
    /// this layer, reporting the honest `Outcome` is that layer's job to
    /// build, not this plugin's job to hide.
    Failed(FailReason),
    /// The plan's last edge finished: the player is at (or has passed through
    /// and stopped in) the goal.
    Arrived,
}

/// Why the most recent search did not reach the goal — [`lodestone_nav::Outcome`]
/// minus the variant that means success, plus the two ways this plugin can fail
/// before a [`Search`] is even built.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum FailReason {
    /// The default before any search has failed — never itself reported.
    #[default]
    None,
    /// `Res<VersionData>` had no adapter compiled in, so there is no
    /// [`lodestone_nav::BlockCensus`] to build a [`FactsTable`] from. Refusing
    /// to plan with no world knowledge is `FactsTable::empty()`'s own
    /// documented policy; this is that policy reaching the plugin layer.
    NoVersionAdapter,
    /// The centre column of the requested snapshot is not loaded
    /// ([`SnapshotView::build`] returned `None`), or nothing standable exists
    /// under the player's current position ([`lodestone_nav::seed_node`]
    /// returned `None` — mid-air, in a wall, over a void).
    NoStart,
    /// The search ran to completion without [`Outcome::Reached`].
    Search(Outcome),
}

/// The in-flight or completed search/plan for the current [`AutopilotGoal`].
///
/// Not `pub`: everything a caller needs is [`AutopilotGoal`] (write) and
/// [`AutopilotStatus`] (read). Exposing the raw [`Search`]/[`Plan`] would let a
/// caller reach into the middle of a resumable search, which nothing needs and
/// which would make "what wrote this tick's intent" harder to audit.
#[derive(Resource, Default)]
struct AutopilotState {
    /// The goal the current search/plan belongs to. Compared against
    /// [`AutopilotGoal`] every tick so a goal change — including the caller
    /// setting the *same* goal again — is what actually needs it to work: this
    /// only clears and restarts when the goal is genuinely different, not
    /// every tick a goal happens to be present.
    for_goal: Option<BlockPos>,
    search: Option<Search>,
    plan: Option<Plan>,
    edge: usize,
    /// Whether `plan`'s own search outcome was [`Outcome::Reached`] — if so
    /// `plan` already ends inside the goal and no continuation will ever be
    /// dispatched for it (`docs/baritone-port.md` §4.9: segmentation exists
    /// for the goal-missing partial case, not the already-arrived one).
    reached_goal: bool,
    /// A search for the *next* segment, dispatched once `plan`'s remaining
    /// cost drops below [`NavPolicy::replan_lead_ticks`] — see [`plan_route`]'s
    /// segmentation block.
    continuation_search: Option<Search>,
    /// The next segment, ready to splice in the moment `plan` runs out.
    continuation_plan: Option<Plan>,
    /// Whether *that* segment's own search reached the goal — carried
    /// alongside `continuation_plan` so splicing it in also correctly updates
    /// [`Self::reached_goal`].
    continuation_reached_goal: bool,
    /// Every witnessed cell of `plan`, sampled the instant it was adopted —
    /// `docs/baritone-port.md` §4.5's witness set, plus the value it read at
    /// commit time so a later re-sample can diff against it
    /// (`lodestone_nav::witness`). Empty exactly when `plan` is `None`.
    witness_baseline: HashMap<u64, u32>,
    /// The same baseline for `continuation_plan`, carried alongside it so
    /// splicing (`drive_plan`) swaps both atomically — the spliced-in plan's
    /// own witnesses, not the plan it replaced.
    continuation_witness_baseline: HashMap<u64, u32>,
    /// Ticks since the last full [`WITNESS_SWEEP_INTERVAL_TICKS`] re-sample of
    /// `plan`'s remaining witness set. Reset whenever `plan` changes.
    ticks_since_witness_sweep: u32,
}

/// (Re)start or resume the search toward [`AutopilotGoal`].
///
/// # Why this steps a resumable search rather than calling `Search::run` once
///
/// `docs/frame-pacing.md`'s rule, restated in `docs/baritone-port.md` §2.2(2):
/// "a client the server considers stalled is sent no chunks at all, silently."
/// A tens-of-thousands-of-node search on the one thread that also produces this
/// tick's movement packet is exactly the stall that rule exists to forbid.
/// [`Budget::PER_TICK`] is sized "large enough that a 20,000-node search
/// finishes in ~10 ticks" (its own doc comment) — spending it one step per tick
/// keeps this system's per-call cost bounded regardless of how far the goal is.
///
/// Chained before [`drive_plan`] and, like it, ordered `.after(TickSet::Intent)`
/// (see the crate docs) even though this system writes no
/// [`MovementIntent`]/[`LookIntent`] itself, only [`AutopilotState`] — the two
/// are one plugin-owned pipeline and keeping them in the same ordering window
/// means a future third stage never has to re-derive where it belongs.
fn plan_route(
    goal: Res<AutopilotGoal>,
    mut state: ResMut<AutopilotState>,
    mut status: ResMut<AutopilotStatus>,
    chunk_world: Res<ChunkWorld>,
    version: Res<VersionData>,
    players: Query<&PhysicsState, With<LocalPlayer>>,
) {
    let Some(target) = goal.0 else {
        if state.for_goal.is_some() {
            *state = AutopilotState::default();
            *status = AutopilotStatus::Idle;
        }
        return;
    };

    // Witness-set invalidation (`docs/baritone-port.md` §4.5) plus the
    // look-ahead window (§4.5/§2.3): before doing anything else this tick,
    // check whether the terrain the *active* plan's legality depended on has
    // changed since it was committed. Two checks, two cadences:
    //
    // - every tick, a cheap look-ahead over the next `LOOKAHEAD_EDGES` edges
    //   — catches a hazard forming just ahead before the body ever reaches
    //   it;
    // - rate-limited (`WITNESS_SWEEP_INTERVAL_TICKS`), a full sweep of the
    //   rest of the plan's own witness set — catches a change further down
    //   the route that the window has not reached yet.
    //
    // A hit forces `need_fresh` below exactly as a goal change does: discard
    // the continuation and replan from the live position, per §4.9's "never
    // execute a plan you already know is stale."
    let mut invalidated_at = None;
    if state.for_goal == Some(target)
        && let Some(plan) = state.plan.as_ref()
        && !state.witness_baseline.is_empty()
    {
        let world = chunk_world.read();
        let window = plan.witnesses_in_range(state.edge..state.edge.saturating_add(LOOKAHEAD_EDGES));
        invalidated_at = window.iter().find_map(|key| {
            let &recorded = state.witness_baseline.get(key)?;
            let node = lodestone_nav::NavNode::unpack(*key)?;
            let live = witness::point_state(&world, node.x, node.y, node.z);
            (live != Some(recorded)).then_some((node.x, node.y, node.z))
        });

        if invalidated_at.is_none() {
            state.ticks_since_witness_sweep = state.ticks_since_witness_sweep.saturating_add(1);
            if state.ticks_since_witness_sweep >= WITNESS_SWEEP_INTERVAL_TICKS {
                state.ticks_since_witness_sweep = 0;
                invalidated_at = witness::first_change(&world, &state.witness_baseline);
            }
        }
    }

    let need_fresh = state.for_goal != Some(target) || invalidated_at.is_some();

    if need_fresh {
        *state = AutopilotState {
            for_goal: Some(target),
            ..AutopilotState::default()
        };

        let Ok(physics) = players.single() else {
            return;
        };
        let Some(adapter) = version.0.as_deref() else {
            *status = AutopilotStatus::Failed(FailReason::NoVersionAdapter);
            return;
        };
        let facts = Arc::new(FactsTable::build(&AdapterCensus(adapter)));
        let seeded = {
            let world = chunk_world.read();
            drive::seed_search(
                &world,
                physics.0.position,
                target,
                facts,
                SNAPSHOT_RADIUS,
                NavPolicy::default(),
            )
        };
        match seeded {
            Some(search) => {
                state.search = Some(search);
                *status = AutopilotStatus::Planning;
            }
            None => *status = AutopilotStatus::Failed(FailReason::NoStart),
        }
    }

    if let Some(search) = state.search.as_mut() {
        match search.step(Budget::PER_TICK) {
            Progress::Working => {}
            Progress::Done(Outcome::Reached) => {
                state.plan = state.search.take().and_then(|s| s.best_plan());
                state.edge = 0;
                state.reached_goal = true;
                sample_witness_baseline(&mut state, &chunk_world);
                *status = if state.plan.is_some() {
                    AutopilotStatus::Driving
                } else {
                    // `best_plan` returning `None` after `Outcome::Reached` would be
                    // a `lodestone-nav` defect, not a reachable case here — but this
                    // plugin refuses rather than unwraps, per this repo's own
                    // "refuse rather than guess" rule.
                    AutopilotStatus::Failed(FailReason::Search(Outcome::Reached))
                };
            }
            Progress::Done(outcome @ (Outcome::BudgetExhausted | Outcome::WorldExhausted)) => {
                // A partial plan that does not yet reach the goal is still a
                // real plan to drive — segmentation's whole premise
                // (`docs/baritone-port.md` §4.9) is that "ran out of snapshot"
                // is progress, not failure, as long as a continuation picks up
                // from where this one ends.
                state.plan = state.search.take().and_then(|s| s.best_plan());
                state.edge = 0;
                state.reached_goal = false;
                sample_witness_baseline(&mut state, &chunk_world);
                *status = if state.plan.is_some() {
                    AutopilotStatus::Driving
                } else {
                    AutopilotStatus::Failed(FailReason::Search(outcome))
                };
            }
            Progress::Done(outcome) => {
                state.search = None;
                *status = AutopilotStatus::Failed(FailReason::Search(outcome));
            }
        }
    }

    // Segmentation (`docs/baritone-port.md` §4.9): once the active plan's
    // remaining cost — excluding the edge currently executing, so one long
    // edge cannot suppress this forever — drops below `replan_lead_ticks`,
    // plan the next segment *now*, while still walking. This is what makes a
    // journey longer than one `SnapshotView` possible at all: without it the
    // bot always stops dead the moment a plan that never reached the goal
    // runs out.
    if !state.reached_goal
        && state.continuation_search.is_none()
        && state.continuation_plan.is_none()
        && let Some(plan) = state.plan.as_ref()
        && plan.remaining_cost_after(state.edge).as_f64() < f64::from(NavPolicy::default().replan_lead_ticks)
    {
        let terminal = plan.terminal();
        if let Some(adapter) = version.0.as_deref() {
            let facts = Arc::new(FactsTable::build(&AdapterCensus(adapter)));
            let world = chunk_world.read();
            // `None` here just means the terminal node's own column has not
            // loaded yet — normal while walking toward a snapshot's edge, and
            // worth trying again next tick rather than treating as failure
            // (`drive::continuation_search`'s own doc comment).
            state.continuation_search =
                drive::continuation_search(&world, terminal, target, facts, SNAPSHOT_RADIUS, NavPolicy::default());
        }
    }

    if let Some(search) = state.continuation_search.as_mut() {
        match search.step(Budget::PER_TICK) {
            Progress::Working => {}
            Progress::Done(outcome) => {
                let reached = outcome == Outcome::Reached;
                if let Some(plan) = state.continuation_search.take().and_then(|s| s.best_plan()) {
                    let world = chunk_world.read();
                    state.continuation_witness_baseline = witness::sample(&world, &plan.witnesses());
                    state.continuation_plan = Some(plan);
                    state.continuation_reached_goal = reached;
                }
                // A continuation with no usable plan (a genuine `Failed`, or
                // `Reached`/`BudgetExhausted`/`WorldExhausted` with no partial
                // clearing `min_progress`) is simply not retried this tick:
                // `continuation_search` is already cleared by `take()` above,
                // so the dispatch block will fire again next tick. This has no
                // rate limit yet (`min_replan_interval_ticks` is declared but
                // not wired to this path) — acceptable for M2's "medium
                // journeys work" bar, and worth revisiting if a goal that is
                // genuinely unreachable past the snapshot edge is found to spin.
            }
        }
    }
}

/// Snapshot `state.plan`'s witness set into `state.witness_baseline`, or
/// clear it when `state.plan` is `None` — the shared tail of both search-done
/// branches above, factored out so the two stay in lockstep (a baseline for
/// the wrong plan, or none at all, would make [`plan_route`]'s invalidation
/// check either blind or spuriously trip on the very first tick).
fn sample_witness_baseline(state: &mut AutopilotState, chunk_world: &ChunkWorld) {
    state.ticks_since_witness_sweep = 0;
    let Some(plan) = state.plan.as_ref() else {
        state.witness_baseline = HashMap::new();
        return;
    };
    let witnesses = plan.witnesses();
    let world = chunk_world.read();
    state.witness_baseline = witness::sample(&world, &witnesses);
}

/// Turn the current plan edge into this tick's [`MovementIntent`]/[`LookIntent`],
/// in `GameTick`, `.after(TickSet::Intent).before(TickSet::Physics)` — see the
/// crate docs for why here and not `.in_set(TickSet::Intent)`.
///
/// Closed-loop every tick, per [`lodestone_nav::WalkDrive::tick`]'s own
/// contract: this reads the player's **actual** state each call, never a
/// reference trajectory, which is what absorbs the small errors
/// `docs/baritone-port.md` §2.3 says otherwise compound into overshoot.
fn drive_plan(
    mut state: ResMut<AutopilotState>,
    mut status: ResMut<AutopilotStatus>,
    mut players: Query<(Entity, &PhysicsState, &mut MovementIntent), With<LocalPlayer>>,
    mut commands: Commands,
) {
    let Ok((entity, physics, mut intent)) = players.single_mut() else {
        return;
    };
    let Some(plan) = state.plan.as_ref() else {
        return;
    };
    let Some(edge) = plan.edges().get(state.edge).copied() else {
        // The plan is exhausted. If a continuation is ready, splice it in and
        // keep driving with no visible stutter: concatenation is valid by
        // construction, since the continuation started at exactly this plan's
        // terminal node (`docs/baritone-port.md` §4.9 — "the new plan
        // therefore begins exactly where the old one ends"). Otherwise: the
        // goal really is reached, or stand still and wait rather than mis-
        // splicing or guessing (§4.9: "a visible pause is strictly better than
        // executing a guess").
        if let Some(next) = state.continuation_plan.take() {
            state.reached_goal = state.continuation_reached_goal;
            state.plan = Some(next);
            state.edge = 0;
            // The spliced-in plan's own witnesses, not the plan it replaced —
            // otherwise the very next invalidation check would compare live
            // terrain against a baseline for cells the new plan never reads.
            state.witness_baseline = std::mem::take(&mut state.continuation_witness_baseline);
            state.ticks_since_witness_sweep = 0;
            // No intent this tick; the spliced plan's first edge drives from
            // the next one, exactly as a freshly-adopted plan would.
            return;
        }
        if state.reached_goal {
            state.plan = None;
            // Keep `witness_baseline`'s "empty exactly when `plan` is `None`"
            // invariant (`AutopilotState`'s own doc comment) — otherwise a
            // stale baseline from the just-finished plan would sit here doing
            // nothing until the next goal, harmless today only because every
            // invalidation check is gated on `state.plan` being `Some` first.
            state.witness_baseline = HashMap::new();
            commands.entity(entity).remove::<LookIntent>();
            *status = AutopilotStatus::Arrived;
        } else {
            // Waiting for a continuation that has not arrived yet: hold
            // still rather than drift on whatever the last edge's script
            // last wrote.
            intent.0 = MovementInput::NONE;
        }
        return;
    };

    let last = state.edge + 1 == plan.len();
    let tick = drive::edge_drive(&edge, last).tick(&physics.0);
    intent.0 = tick.input;
    commands.entity(entity).insert(LookIntent {
        yaw: tick.yaw,
        pitch: 0.0,
    });

    if drive::edge_drive(&edge, last).done(&physics.0) {
        state.edge += 1;
    }
}

/// Vertical offset above a waypoint's [`Edge::to_surface`](lodestone_nav::Edge::to_surface)
/// a marker floats at, in blocks — enough to clear the block itself rather
/// than sitting flush with (and half-clipped by) the ground the plan's own
/// height already accounts for.
const WAYPOINT_HEIGHT: f64 = 1.2;

/// A waypoint marker's footprint, in blocks — small enough that a run of
/// them along a corridor reads as a trail rather than a wall.
const WAYPOINT_SIZE: [f32; 2] = [0.4, 0.4];

/// A saturated cyan with no vanilla block or particle using it, so a
/// waypoint marker is unambiguous at a glance. [`PluginTexture::Solid`]
/// makes this the billboard's entire visible colour (see that variant's own
/// doc for why this plugin does not use [`PluginTexture::Named`] here: an
/// item id like a compass would resolve against an atlas this render pass
/// does not bind and silently fall back to a flat tint anyway).
const WAYPOINT_COLOR: [f32; 4] = [0.1, 0.9, 1.0, 0.9];

/// `Extract` / `ExtractSet::Debug`: one [`PluginBillboard`] per **remaining**
/// edge of the active plan — the producer half of the textured/billboard
/// world-space draw channel (a generalization of the debug-line pipeline that
/// still never lets a `wgpu::Device` cross the plugin boundary).
/// `lodestone_ecs::plugin_draw`'s own module doc names "a
/// pathfinder's planned route" as the channel's reason to exist; before this
/// system, nothing in the tree actually pushed one (every reference was
/// infrastructure — the render pipeline, the resource, the wire — with zero
/// producers, the `ClientAction::SetFlying` shape inverted).
///
/// Only `state.edge..` — the edges not yet walked — so the marker trail
/// shrinks as the bot advances rather than also drawing the route already
/// behind it (which [`Plan::witnesses_in_range`]'s own look-ahead-window
/// reasoning already treats as the uninteresting half: nothing upcoming
/// depends on it). Reading [`AutopilotState`] directly (not
/// [`AutopilotStatus`]) means a caller sees the *real* geometry the executor
/// is driving, not a status caller's guess at it.
fn extract_plan_billboards(state: Res<AutopilotState>, mut billboards: ResMut<PluginBillboards>) {
    let Some(plan) = state.plan.as_ref() else {
        return;
    };
    for edge in plan.edges().iter().skip(state.edge) {
        billboards.0.push(PluginBillboard {
            position: Vec3d::new(
                f64::from(edge.to.x) + 0.5,
                edge.to_surface + WAYPOINT_HEIGHT,
                f64::from(edge.to.z) + 0.5,
            ),
            size: WAYPOINT_SIZE,
            color: WAYPOINT_COLOR,
            texture: PluginTexture::Solid,
        });
    }
}

/// Registers [`AutopilotGoal`]/[`AutopilotStatus`] and the two [`GameTick`]
/// systems, plus [`extract_plan_billboards`] in `Extract`'s
/// [`ExtractSet::Debug`].
///
/// Does **not** register `lodestone_ecs::CorePlugin` or
/// `lodestone_ecs::player::LocalPlayerPlugin` — a plugin depends on the
/// engine's ordering anchors and components, never installs the engine
/// (`docs/plugin-api.md`'s "what belongs" boundary, restated in
/// `crates/plugins/README.md`). That includes [`PluginBillboards`] itself:
/// `LocalPlayerPlugin` is what `init_resource`s it and clears it
/// `.before(ExtractSet::Debug)` every frame, exactly as it already does for
/// [`MovementIntent`]/[`LookIntent`] — this plugin only ever appends.
#[derive(Debug, Default)]
pub struct AutopilotPlugin;

impl Plugin for AutopilotPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AutopilotGoal>();
        app.init_resource::<AutopilotStatus>();
        app.init_resource::<AutopilotState>();
        app.add_systems(
            GameTick,
            (plan_route, drive_plan)
                .chain()
                .after(TickSet::Intent)
                .before(TickSet::Physics),
        );
        app.add_systems(Extract, extract_plan_billboards.in_set(ExtractSet::Debug));
    }
}
