//! A queued block-edit plugin driving `BreakIntent`/`PlaceIntent`
//! (`lodestone_ecs::player`, `docs/plugin-api.md`'s "intent doctrine"): the
//! first production producer of either component.
//!
//! # What this closes
//!
//! Both components existed, and both had a real consumer
//! (`lodestone_shell::interact::drive_mining`/`drive_placement`, proved end
//! to end by that crate's own `break_intent.rs`/`place_intent.rs`), but
//! nothing outside a unit test ever constructed one — `grep -rn "insert(BreakIntent\|insert(PlaceIntent"`
//! across the tree found exactly two hits, both `#[cfg(test)]`. A type with a
//! real consumer and zero producers is still an island; this crate is the
//! producer half.
//!
//! Same role in this crate group as [`lodestone-mob-spawner`](https://docs.rs)
//! plays for `spawn_entity`/`despawn_entity`: a small, real request queue a
//! caller (a bot script, a chat-command handler, a future
//! `lodestone-nav`-style planner once it grows Break/Place edges) drives from
//! outside the `World`, proving the seam is reachable through the ordinary
//! `App::add_plugins` composition path rather than only from
//! `lodestone-ecs`'s own fixtures.
//!
//! # How it works
//!
//! [`BlockJobQueue`] is a plain public [`bevy_ecs::resource::Resource`], the
//! same shape `lodestone-mob-spawner`'s `SpawnRequests` uses: a caller pushes
//! a [`BlockJob`] with [`BlockJobQueue::submit`] and later drains completed
//! ones with [`BlockJobQueue::drain_finished`]. [`drive_block_jobs`]
//! (`TickSet::Intent`, so it runs strictly before the shell's
//! `TickSet::Send`, where `drive_mining`/`drive_placement` actually consult
//! whatever it installed) runs **one job at a time**:
//!
//! * No active job, a job pending: install the matching intent
//!   ([`BreakIntent`] or [`PlaceIntent`]) on the [`LocalPlayer`] entity.
//! * An active [`BlockJob::Break`]: poll [`BreakOutcome`]. `Progressing`
//!   means keep waiting — a dig is continuous, per that component's own
//!   docs. Anything else (`Idle` or `Rejected`) means the shell has nothing
//!   further to report on this attempt (see `BreakOutcome`'s own docs on why
//!   a broken block's now-air target reads as "nothing more to report"
//!   rather than a fresh `Progressing`): remove [`BreakIntent`] and record a
//!   [`FinishedJob`].
//! * An active [`BlockJob::Place`]: poll whether [`PlaceIntent`] is still
//!   present. A placement is one-shot and the shell removes the component
//!   itself the instant it resolves (see that component's own docs), so its
//!   absence *is* the acknowledgement — record the [`PlaceOutcome`] status
//!   at that point as the [`FinishedJob`].
//!
//! One job at a time, never two intents installed together — a plugin
//! author extending this to a real pathfinder's Break/Place edges (still
//! unimplemented in `lodestone-nav`, which is `Walk`-only today) would want
//! the identical discipline, since `BreakIntent`/`PlaceIntent` are each a
//! single optional component: installing a second one before the first
//! resolves would simply overwrite it, not queue it.
//!
//! # What this does not do
//!
//! It does not reimplement `drive_mining`/`drive_placement`'s own legality
//! or timing — that is exactly the "exactly one system owns each machine"
//! clause `docs/plugin-api.md` names, and this crate's own tests treat the
//! shell's outcome contract as a given boundary (writing `BreakOutcome`/
//! `PlaceOutcome` by hand, the same way a real session's `TickSet::Send`
//! would) rather than re-deriving mining ticks or placement geometry, which
//! `lodestone_shell`'s own hermetic harness (`break_intent.rs`/
//! `place_intent.rs`) already gates.
//!
//! It also is not shipped in the client, deliberately — same category as
//! `lodestone-autopilot`/`lodestone-key-toggle`: a client does not run a
//! digging bot for a player who never asked for one. The route in is the
//! documented one, `App::add_plugins(BlockJobsPlugin)`.
//!
//! # Dependencies
//!
//! `lodestone-ecs` (the intent doctrine, `LocalPlayerPlugin`, `TickSet`),
//! `lodestone-model` (`BlockPos`, `BlockFace`), `bevy_ecs`/`bevy_app` directly
//! (see this crate's `Cargo.toml` for why a plugin deriving its own
//! `Resource` needs them as a direct dependency, not only reachable through
//! `lodestone_ecs::{ecs, app}`).

use std::collections::VecDeque;

use bevy_ecs::query::With;
use bevy_ecs::resource::Resource;
use bevy_ecs::schedule::IntoScheduleConfigs;
use bevy_ecs::system::{Commands, Query, ResMut};
use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::player::{
    BreakIntent, BreakOutcome, BreakStatus, PlaceIntent, PlaceOutcome, PlaceStatus,
};
use lodestone_ecs::{GameTick, LocalPlayer, LocalPlayerPlugin, TickSet};
use lodestone_model::{BlockFace, BlockPos};

/// One queued block edit: mine or place, at a position and the face to
/// approach it from — exactly the two facts [`BreakIntent`]/[`PlaceIntent`]
/// carry, see either component's own docs for why nothing else belongs
/// here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockJob {
    /// Mirrors [`BreakIntent`].
    Break { pos: BlockPos, face: BlockFace },
    /// Mirrors [`PlaceIntent`].
    Place { pos: BlockPos, face: BlockFace },
}

/// How a settled job's attempt resolved — [`BreakStatus`]/[`PlaceStatus`]
/// verbatim, never summarised to a bare success/failure bool, for the same
/// "refusal is always observable, typed" reason `docs/plugin-api.md` names
/// for the components themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobOutcome {
    Break(BreakStatus),
    Place(PlaceStatus),
}

/// A job this plugin is done reporting on, paired with how it settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinishedJob {
    pub job: BlockJob,
    pub outcome: JobOutcome,
}

/// The job currently installed on the local player, and which verb it is —
/// [`drive_block_jobs`]'s only private state, so a caller draining
/// [`BlockJobQueue::drain_finished`] never races the system's own bookkeeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Active {
    Break(BlockJob),
    Place(BlockJob),
}

/// The public queue: a caller pushes work with [`Self::submit`], and this
/// crate's own [`drive_block_jobs`] system runs it one job at a time through
/// the real intent doctrine. Plain [`Resource`], the same shape
/// `lodestone-mob-spawner`'s `SpawnRequests` uses — a caller reaches this
/// with `world.resource_mut::<BlockJobQueue>()`, no `Arc<Mutex<_>>` needed
/// since (unlike `lodestone-event-logger`'s `EventLog`) nothing here needs to
/// be read from outside the `World` between ticks.
#[derive(Resource, Debug, Default)]
pub struct BlockJobQueue {
    pending: VecDeque<BlockJob>,
    active: Option<Active>,
    finished: Vec<FinishedJob>,
}

impl BlockJobQueue {
    /// Queues `job`. Runs after every job ahead of it in submission order —
    /// see [`drive_block_jobs`]'s own docs for why only one intent is ever
    /// installed at a time.
    pub fn submit(&mut self, job: BlockJob) {
        self.pending.push_back(job);
    }

    /// Every job that has settled since the last call, in the order they
    /// finished. Draining (not cloning) so a caller polling every tick never
    /// sees the same [`FinishedJob`] twice.
    pub fn drain_finished(&mut self) -> Vec<FinishedJob> {
        std::mem::take(&mut self.finished)
    }

    /// How many jobs are queued behind the active one (not counting the
    /// active job itself).
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Whether nothing is installed and nothing is queued — the queue has
    /// caught up with every [`Self::submit`] call so far.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.active.is_none() && self.pending.is_empty()
    }
}

/// `TickSet::Intent`: resolves whatever job is active (by polling last
/// tick's [`BreakOutcome`]/[`PlaceIntent`] presence), then, if nothing is
/// active, installs the next pending job's intent — see this crate's own
/// doc for the full state machine.
///
/// Ordered in `TickSet::Intent`, strictly before `TickSet::Send`
/// (`lodestone_shell::interact::drive_mining`/`drive_placement`'s own set),
/// so an intent installed this tick is consulted this same tick — the
/// identical anchor `lodestone-key-toggle`'s own system doc names for
/// "anything shaped like this one".
fn drive_block_jobs(
    mut commands: Commands,
    mut queue: ResMut<BlockJobQueue>,
    players: Query<
        (bevy_ecs::entity::Entity, &BreakOutcome, Option<&PlaceIntent>, &PlaceOutcome),
        With<LocalPlayer>,
    >,
) {
    let Ok((entity, break_outcome, place_intent, place_outcome)) = players.single() else {
        // No local player yet (a fresh `World` before `spawn_local_player`
        // ran) — nothing to drive against. Jobs stay queued for the tick a
        // local player exists.
        return;
    };

    if let Some(active) = queue.active {
        let settled = match active {
            Active::Break(job) => match break_outcome.0 {
                // A dig is continuous — see `BreakOutcome`'s own docs.
                // Nothing to report yet.
                BreakStatus::Progressing => None,
                other => Some(FinishedJob {
                    job,
                    outcome: JobOutcome::Break(other),
                }),
            },
            Active::Place(job) => {
                if place_intent.is_some() {
                    // Still installed: the shell has not resolved this
                    // attempt yet (or a human interposed — see
                    // `PlaceIntent`'s own docs on human priority). Either
                    // way there is nothing to report this tick.
                    None
                } else {
                    // Gone: `drive_placement` removes `PlaceIntent` the
                    // instant it resolves an attempt, whatever the result —
                    // that removal *is* the acknowledgement.
                    Some(FinishedJob {
                        job,
                        outcome: JobOutcome::Place(place_outcome.status),
                    })
                }
            }
        };
        if let Some(settled) = settled {
            if matches!(settled.job, BlockJob::Break { .. }) {
                commands.entity(entity).remove::<BreakIntent>();
            }
            queue.finished.push(settled);
            queue.active = None;
        }
    }

    if queue.active.is_none()
        && let Some(job) = queue.pending.pop_front()
    {
        match job {
            BlockJob::Break { pos, face } => {
                commands.entity(entity).insert(BreakIntent { pos, face });
                queue.active = Some(Active::Break(job));
            }
            BlockJob::Place { pos, face } => {
                commands.entity(entity).insert(PlaceIntent { pos, face });
                queue.active = Some(Active::Place(job));
            }
        }
    }
}

/// Installs [`BlockJobQueue`] and [`drive_block_jobs`], adding
/// [`LocalPlayerPlugin`] first if a caller has not already — the same
/// "add my dependency if missing" guard `lodestone-key-toggle`'s
/// `KeyTogglePlugin::build` uses for the identical reason: `BreakOutcome`/
/// `PlaceOutcome` only exist once `spawn_local_player` has run on an entity
/// carrying `LocalPlayerPlugin`'s component set.
#[derive(Debug, Default)]
pub struct BlockJobsPlugin;

impl Plugin for BlockJobsPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<LocalPlayerPlugin>() {
            app.add_plugins(LocalPlayerPlugin);
        }
        app.init_resource::<BlockJobQueue>();
        app.add_systems(GameTick, drive_block_jobs.in_set(TickSet::Intent));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_ecs::player::{BreakRejection, PlaceRejection, spawn_local_player};
    use lodestone_physics::{PlayerState, Vec3d};

    fn app_with_player() -> (App, bevy_ecs::entity::Entity) {
        let mut app = App::new();
        app.add_plugins(BlockJobsPlugin);
        let entity = spawn_local_player(app.world_mut(), PlayerState::at(Vec3d::new(0.0, 64.0, 0.0), 0.0));
        (app, entity)
    }

    /// **The gate.** Submitting a break job through the public queue must
    /// itself, through a real `GameTick`, write a real `BreakIntent` onto
    /// the local player entity — the exact fact this crate exists to
    /// establish: `BreakIntent`/`PlaceIntent` are the sanctioned write-side
    /// route, and this is a real production consumer rather than a component
    /// with only test call sites.
    #[test]
    fn submitting_a_break_job_installs_a_real_break_intent() {
        let (mut app, entity) = app_with_player();
        let job = BlockJob::Break {
            pos: BlockPos::new(3, 4, 5),
            face: BlockFace::Up,
        };
        app.world_mut().resource_mut::<BlockJobQueue>().submit(job);

        assert!(
            app.world().get::<BreakIntent>(entity).is_none(),
            "nothing must be installed before a GameTick has run"
        );
        app.world_mut().run_schedule(GameTick);

        let installed = app
            .world()
            .get::<BreakIntent>(entity)
            .expect("drive_block_jobs must install BreakIntent from the queued job");
        assert_eq!(installed.pos, BlockPos::new(3, 4, 5));
        assert_eq!(installed.face, BlockFace::Up);
        assert_eq!(
            app.world().resource::<BlockJobQueue>().pending_len(),
            0,
            "the submitted job must have left the pending queue"
        );
    }

    /// A `Progressing` `BreakOutcome` (the shape a real `drive_mining` would
    /// leave mid-dig) must not be treated as finished, and the intent must
    /// stay installed — a dig is continuous, not a one-shot poll.
    #[test]
    fn a_progressing_break_outcome_keeps_the_intent_installed_and_reports_nothing() {
        let (mut app, entity) = app_with_player();
        app.world_mut()
            .resource_mut::<BlockJobQueue>()
            .submit(BlockJob::Break {
                pos: BlockPos::new(1, 2, 3),
                face: BlockFace::North,
            });
        app.world_mut().run_schedule(GameTick);
        assert!(app.world().get::<BreakIntent>(entity).is_some());

        // Simulate the shell's own consumer having progressed the dig —
        // the boundary this crate's own tests treat as given, per this
        // module's "what this does not do" doc.
        app.world_mut().get_mut::<BreakOutcome>(entity).unwrap().0 = BreakStatus::Progressing;
        app.world_mut().run_schedule(GameTick);

        assert!(
            app.world().get::<BreakIntent>(entity).is_some(),
            "a Progressing outcome must leave BreakIntent installed"
        );
        assert!(
            app.world_mut()
                .resource_mut::<BlockJobQueue>()
                .drain_finished()
                .is_empty(),
            "nothing has finished yet"
        );
    }

    /// **The completion path.** Once the simulated outcome moves off
    /// `Progressing` (here: rejected, the shape a break-turned-air target
    /// leaves per `BreakOutcome`'s own docs), the job must be reported
    /// finished with that exact status, `BreakIntent` must be removed, and
    /// the next queued job must start on the very same tick.
    #[test]
    fn a_settled_break_outcome_finishes_the_job_and_starts_the_next_one() {
        let (mut app, entity) = app_with_player();
        let first = BlockJob::Break {
            pos: BlockPos::new(1, 2, 3),
            face: BlockFace::North,
        };
        let second = BlockJob::Place {
            pos: BlockPos::new(9, 9, 9),
            face: BlockFace::East,
        };
        {
            let mut queue = app.world_mut().resource_mut::<BlockJobQueue>();
            queue.submit(first);
            queue.submit(second);
        }
        app.world_mut().run_schedule(GameTick);
        assert!(app.world().get::<BreakIntent>(entity).is_some());
        assert!(
            app.world().get::<PlaceIntent>(entity).is_none(),
            "only one intent may be installed at a time"
        );

        app.world_mut().get_mut::<BreakOutcome>(entity).unwrap().0 =
            BreakStatus::Rejected(BreakRejection::UnreachableOrObstructed);
        app.world_mut().run_schedule(GameTick);

        assert!(
            app.world().get::<BreakIntent>(entity).is_none(),
            "a settled job must remove its own BreakIntent"
        );
        let finished = app.world_mut().resource_mut::<BlockJobQueue>().drain_finished();
        assert_eq!(
            finished,
            vec![FinishedJob {
                job: first,
                outcome: JobOutcome::Break(BreakStatus::Rejected(
                    BreakRejection::UnreachableOrObstructed
                )),
            }],
            "the finished batch must carry the exact status the outcome held, not a summary"
        );
        assert!(
            app.world().get::<PlaceIntent>(entity).is_some(),
            "the second queued job must start the same tick the first one finished"
        );
    }

    /// The placement mirror: a queued place job installs `PlaceIntent`, and
    /// once the shell's own one-shot removal (simulated here) has happened,
    /// the job is reported finished with whatever `PlaceOutcome` was left —
    /// the component's absence, not a status enum value, is what signals
    /// completion, matching `PlaceIntent`'s own "removal is the
    /// acknowledgement" contract.
    #[test]
    fn a_place_job_finishes_when_the_shell_removes_place_intent() {
        let (mut app, entity) = app_with_player();
        let job = BlockJob::Place {
            pos: BlockPos::new(7, 8, 9),
            face: BlockFace::Down,
        };
        app.world_mut().resource_mut::<BlockJobQueue>().submit(job);
        app.world_mut().run_schedule(GameTick);
        assert!(app.world().get::<PlaceIntent>(entity).is_some());

        // Simulate `drive_placement` having resolved and removed it —
        // exactly the two effects that component's own docs say happen
        // together on the same tick.
        app.world_mut().entity_mut(entity).remove::<PlaceIntent>();
        app.world_mut().get_mut::<PlaceOutcome>(entity).unwrap().status =
            PlaceStatus::Rejected(PlaceRejection::NothingPlaceableHeld);
        app.world_mut().run_schedule(GameTick);

        let finished = app.world_mut().resource_mut::<BlockJobQueue>().drain_finished();
        assert_eq!(
            finished,
            vec![FinishedJob {
                job,
                outcome: JobOutcome::Place(PlaceStatus::Rejected(
                    PlaceRejection::NothingPlaceableHeld
                )),
            }]
        );
        assert!(app.world_mut().resource_mut::<BlockJobQueue>().is_idle());
    }

    /// **The control.** With no `BlockJobsPlugin`-driven job ever submitted,
    /// a real `GameTick` must install nothing and finish nothing — proving
    /// the gate above is discriminating on a *submitted* job, not merely on
    /// "a local player exists and a schedule ran."
    #[test]
    fn an_empty_queue_installs_nothing() {
        let (mut app, entity) = app_with_player();
        app.world_mut().run_schedule(GameTick);
        assert!(app.world().get::<BreakIntent>(entity).is_none());
        assert!(app.world().get::<PlaceIntent>(entity).is_none());
        assert!(app.world().resource::<BlockJobQueue>().is_idle());
    }
}
