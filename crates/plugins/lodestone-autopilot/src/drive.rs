//! The two small pieces of glue between [`lodestone_nav`] and this plugin's
//! systems: turning a `(world, position, goal)` triple into a seeded
//! [`Search`], and turning a [`lodestone_nav::Edge`] into the
//! [`lodestone_nav::WalkDrive`] that produces this tick's keys.
//!
//! Kept as plain functions, not inlined into the systems in `lib.rs`, for the
//! same reason `lodestone-nav` itself is a plain library
//! (`docs/baritone-port.md` §4.0): a function with no `Res`/`Query` parameters
//! is callable from a hermetic test with a hand-built [`lodestone_world::World`]
//! and no `bevy_ecs::World` at all — see `tests/drives_to_goal.rs`'s
//! [`compute_plan`] use, which is exactly [`seed_search`] plus running the
//! search to completion rather than one [`lodestone_nav::Budget::PER_TICK`]
//! step at a time.

use std::sync::Arc;

use lodestone_model::BlockPos;
use lodestone_nav::{
    AtBlock, Budget, Edge, FactsTable, Goal, NavNode, NavPolicy, Outcome, Plan, Search,
    SnapshotView, WalkDrive,
};
use lodestone_physics::{PhysicsProfile, Vec3d};
use lodestone_world::World;

/// Build a [`Search`] toward `goal`'s block, given an already-built `view` and
/// its `start` node.
///
/// Shared by [`seed_search`] (view centred on a live position, start derived
/// from [`lodestone_nav::seed_node`]) and [`continuation_search`] (view
/// centred on a plan's own terminal node, which **is** the start) — the two
/// differ only in how the view and start are obtained, never in how the
/// search itself is built from them, which is what keeps a continuation's
/// search indistinguishable from a fresh one to everything downstream.
fn search_from(view: SnapshotView, start: NavNode, goal: BlockPos, policy: NavPolicy) -> Search {
    let goal: Box<dyn Goal> = Box::new(AtBlock {
        x: goal.x,
        y: goal.y,
        z: goal.z,
    });
    Search::new(Arc::new(view), start, goal, policy, PhysicsProfile::mc_1_21())
}

/// Build a [`Search`] from `position` toward `goal`'s block, snapshotting
/// `radius` columns of `world` around `position`'s column.
///
/// `None` when the centre column is not loaded ([`SnapshotView::build`]) or
/// nothing standable exists under `position` ([`lodestone_nav::seed_node`]) —
/// the same two honest-refusal cases [`AutopilotStatus::Failed`]'s
/// [`crate::FailReason::NoStart`] reports.
///
/// [`AutopilotStatus::Failed`]: crate::AutopilotStatus::Failed
#[must_use]
pub fn seed_search(
    world: &World,
    position: Vec3d,
    goal: BlockPos,
    facts: Arc<FactsTable>,
    radius: i32,
    policy: NavPolicy,
) -> Option<Search> {
    #[allow(clippy::cast_possible_truncation)]
    let (cx, cz) = (position.x.floor() as i32, position.z.floor() as i32);
    let view = SnapshotView::build(world, cx, cz, radius, facts)?;
    let start = lodestone_nav::seed_node(&view, position)?;
    Some(search_from(view, start, goal, policy))
}

/// Build a [`Search`] continuing a plan from its own terminal node —
/// segmentation's mechanism for a journey longer than one snapshot
/// (`docs/baritone-port.md` §4.9). Snapshots `radius` columns of `world`
/// around the terminal node's own column and starts the search there, with
/// its `Arrival` carried over unchanged, rather than re-deriving a start node
/// from the player's live position: a continuation plans *ahead* of where the
/// body currently is, and concatenation is only valid when the new plan
/// begins at exactly the position (and momentum state) the old one ends at.
///
/// `None` only when the terminal node's own column is not loaded — normal
/// while walking toward the edge of a snapshot that has not caught up yet,
/// not a search failure; the caller should simply try again on a later tick.
#[must_use]
pub fn continuation_search(
    world: &World,
    terminal: NavNode,
    goal: BlockPos,
    facts: Arc<FactsTable>,
    radius: i32,
    policy: NavPolicy,
) -> Option<Search> {
    let view = SnapshotView::build(world, terminal.x, terminal.z, radius, facts)?;
    Some(search_from(view, terminal, goal, policy))
}

/// [`seed_search`] plus running the search to completion in one call.
///
/// This plugin's own [`crate::plan_route`] never calls this — it steps a
/// [`Search`] one [`Budget::PER_TICK`] at a time across ticks, per that
/// system's doc comment on why a search must not block a frame. This function
/// exists for callers that are not a per-tick system: a hermetic test with a
/// short, fast search (see `tests/drives_to_goal.rs`), or a future offline
/// "plan this route" tool. `budget_nodes` is a hard ceiling on total node
/// expansions, so a caller that accidentally points this at an unreachable
/// goal cannot hang.
#[must_use]
pub fn compute_plan(
    world: &World,
    position: Vec3d,
    goal: BlockPos,
    facts: Arc<FactsTable>,
    radius: i32,
    policy: NavPolicy,
    budget_nodes: u32,
) -> Option<Plan> {
    let mut search = seed_search(world, position, goal, facts, radius, policy)?;
    match search.run(Budget {
        nodes: budget_nodes,
    }) {
        Outcome::Reached => search.best_plan(),
        _ => None,
    }
}

/// The [`WalkDrive`] one plan `edge` produces, braking only when it is the
/// plan's `last` edge — see [`WalkDrive`]'s own doc comment on why a mid-plan
/// edge must not brake (it would stutter once per block instead of walking
/// continuously).
///
/// # M2: `Walk`/`StepUp`/`Descend`/`Drop` all still fit one script
///
/// `lodestone-nav`'s M1 comment here said this `match` would need a new arm
/// the moment a second `MoveKind` landed, as a forcing function against
/// silently mis-driving a kind `WalkDrive` cannot express. It was right to
/// force the check, and the answer for M2's three new kinds turned out to be
/// "no new script needed": `StepUp`/`Descend`/`Drop` all still aim at the
/// destination cell's centre and either brake or don't exactly like `Walk` —
/// the only physical difference is whether a jump is needed to clear the
/// ascent, which `WalkDrive::jump` already carries as a plain flag. A kind
/// that needs genuinely different keys — `Climb`, holding a direction key
/// against a ladder rather than aiming at a cell centre — is the one that will
/// actually need a second script, and *that* is the arm this match should grow
/// next.
#[must_use]
pub fn edge_drive(edge: &Edge, last: bool) -> WalkDrive {
    let jump = matches!(edge.kind, lodestone_nav::MoveKind::StepUp(_));
    WalkDrive {
        cell: [edge.to.x, edge.to.y, edge.to.z],
        surface: edge.to_surface,
        brake: last,
        sprint: false,
        steer: true,
        jump,
    }
}
