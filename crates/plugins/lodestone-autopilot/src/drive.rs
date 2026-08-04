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
use lodestone_nav::{AtBlock, Budget, Edge, FactsTable, Goal, NavPolicy, Outcome, Plan, Search, SnapshotView, WalkDrive};
use lodestone_physics::{PhysicsProfile, Vec3d};
use lodestone_world::World;

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
    let goal: Box<dyn Goal> = Box::new(AtBlock {
        x: goal.x,
        y: goal.y,
        z: goal.z,
    });
    Some(Search::new(
        Arc::new(view),
        start,
        goal,
        policy,
        PhysicsProfile::mc_1_21(),
    ))
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
/// M1 only: every [`Edge::kind`](lodestone_nav::MoveKind) `lodestone-nav`
/// admits today is [`lodestone_nav::MoveKind::Walk`], so this has nothing to
/// match on yet. When `WalkDiagonal`/`StepUp`/`Descend`/… land (M2+), this is
/// the one place that grows a second arm.
#[must_use]
pub fn edge_drive(edge: &Edge, last: bool) -> WalkDrive {
    let lodestone_nav::MoveKind::Walk(_) = edge.kind;
    WalkDrive {
        cell: [edge.to.x, edge.to.y, edge.to.z],
        surface: edge.to_surface,
        brake: last,
        sprint: false,
        steer: true,
    }
}
