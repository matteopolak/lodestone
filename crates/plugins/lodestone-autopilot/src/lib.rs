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

use std::sync::Arc;

use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::ecs::prelude::{Commands, Entity, Query, Res, ResMut, With};
use lodestone_ecs::ecs::resource::Resource;
use lodestone_ecs::ecs::schedule::IntoScheduleConfigs;
use lodestone_ecs::player::PhysicsState;
use lodestone_ecs::{
    ChunkWorld, GameTick, LocalPlayer, LookIntent, MovementIntent, TickSet, VersionData,
};
use lodestone_model::BlockPos;
use lodestone_nav::{AdapterCensus, Budget, FactsTable, NavPolicy, Outcome, Plan, Progress, Search};

pub mod drive;

pub use drive::compute_plan;

/// How many columns in every horizontal direction a search snapshot spans
/// (`docs/baritone-port.md` §4.2 defaults this to 12 for a full 25×25; M1's
/// short-range demo use case does not need that much, and a smaller snapshot
/// is a smaller [`SnapshotView::build`] copy every time the goal changes).
pub const SNAPSHOT_RADIUS: i32 = 8;

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

    if state.for_goal != Some(target) {
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
            Progress::Done(outcome) => {
                state.search = None;
                *status = AutopilotStatus::Failed(FailReason::Search(outcome));
            }
        }
    }
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
        // The plan is exhausted: hand rotation back to mouse-look and let
        // `plan_route` decide what happens next (a fresh goal, or nothing).
        state.plan = None;
        commands.entity(entity).remove::<LookIntent>();
        *status = AutopilotStatus::Arrived;
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

/// Registers [`AutopilotGoal`]/[`AutopilotStatus`] and the two [`GameTick`]
/// systems.
///
/// Does **not** register `lodestone_ecs::CorePlugin` or
/// `lodestone_ecs::player::LocalPlayerPlugin` — a plugin depends on the
/// engine's ordering anchors and components, never installs the engine
/// (`docs/plugin-api.md`'s "what belongs" boundary, restated in
/// `crates/plugins/README.md`).
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
    }
}
