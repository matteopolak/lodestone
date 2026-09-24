//! Runtime attach/detach of the presentation-only ECS systems.
//!
//! [`Sim::client_app`](super::Sim::client_app)'s own doc comment names the
//! four plugins it adds on top of `lodestone_app::client_app` — render-side
//! entity interpolation, the `Display`-family extract, the terrain mesher and
//! the pick/interaction/particle systems — and says why: "all three exist to
//! feed a renderer." [`PresentationSet`] is that boundary made real: every
//! system those four plugins register is tagged into it, so
//! [`detach`]/[`attach`] can remove and restore the whole group as one unit
//! rather than four separately-tracked lists.
//!
//! # Why this is a set removal, not four plugin removals
//!
//! `bevy_ecs::schedule::Schedule::remove_systems_in_set` is keyed on a
//! [`SystemSet`], not on individual systems or on a `Plugin` — see
//! `docs/runtime-presentation.md`. And a `Plugin::build` cannot simply be
//! re-run to restore what was removed: `Sim` takes the `World` out of the
//! `App` at construction and drops the `App` itself (`sim/build.rs`'s
//! `adopt`), so there is no `App` left to call `add_plugins` through, and
//! **`add_systems` does not deduplicate** — calling `build` a second time on
//! a schedule that still holds the old copies would run every presentation
//! system twice. [`attach`] instead calls each plugin's own
//! `add_presentation_systems(&mut World)` — the same systems, tagged the same
//! way, added straight to the `World`'s `Schedules` resource with no `App` in
//! the loop — which is safe to call only because [`detach`] is *exact*: it
//! removes every one of those systems from every schedule it was added to
//! before `attach` ever runs again.

use bevy_ecs::schedule::SystemSet;

/// Marks every system that exists only to feed a renderer: render-side entity
/// interpolation ([`crate::entities::EntityInterpPlugin`]), the
/// `Display`-family extract ([`crate::display_entities::DisplayEntityPlugin`]),
/// the terrain mesher ([`crate::mesher::TerrainPlugin`]) and the
/// pick/interaction/particle systems ([`crate::interact::InteractPlugin`]).
///
/// Nothing outside those four plugins' own `add_presentation_systems`
/// functions is ever tagged with this set.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PresentationSet;

#[cfg(feature = "runtime-presentation")]
mod runtime {
    use super::PresentationSet;
    use bevy_ecs::schedule::{ScheduleCleanupPolicy, ScheduleError, ScheduleLabel};
    use bevy_ecs::world::World;
    use lodestone_ecs::{Extract, GameTick, Update};

    /// Remove every presentation system from every schedule
    /// [`super::attach`]/[`crate::sim::Sim::client_app`] put one in, using
    /// [`ScheduleCleanupPolicy::RemoveSystemsOnly`] — one of the two
    /// edge-repairing policies (it re-adds transitive dependency edges across
    /// the hole, so a surviving system ordered `.after`/`.before` a
    /// presentation system keeps that ordering rather than silently losing
    /// it). Returns the total number of systems removed, across all three
    /// schedules.
    ///
    /// **Not [`ScheduleCleanupPolicy::RemoveSetAndSystems`]**, even though
    /// that is the crate's own default and the more obvious reading of "also
    /// remove the set": measured against this session's real `Update`
    /// schedule, it panics inside `bevy_ecs` 0.19.1's own
    /// `SystemSets::check_type_set_ambiguity` the next time that schedule is
    /// (re)built — `System set with key SystemSetKey(..) does not exist in
    /// the schedule` — reproducibly, on the very first detach of a freshly
    /// constructed session, before this crate's own removal logic runs at
    /// all (the panic is inside `Schedule::initialize`'s pre-check, not
    /// inside anything `sim::presentation` does). `RemoveSystemsOnly` skips
    /// only the `remove_set_by_key` step — the set node itself survives,
    /// empty, and [`attach`]'s `.in_set(PresentationSet)` calls simply
    /// re-populate it — and does not trip the same path. Both policies are
    /// "edge-repairing" in the sense this issue asks for; this is a choice
    /// between the two, not a fallback to a lossy one.
    ///
    /// `Update`, `GameTick` and `Extract` are the three schedules the four
    /// plugins register into (see each plugin's own `add_presentation_systems`).
    /// A schedule that never held the set — nothing installed it, or a
    /// previous `detach` already ran — reports `Err(SetNotFound)` or is
    /// simply absent from `Schedules` (a harness that only added a subset of
    /// the four plugins may never have created it), both tolerated per
    /// schedule as "nothing to remove here" rather than asserted.
    ///
    /// Goes through [`World::try_schedule_scope`] — which removes only the
    /// *one named schedule* from the `Schedules` resource, leaving that
    /// resource itself present in the `World` for the closure's duration —
    /// rather than a raw `World::resource_scope::<Schedules, _>`, which
    /// removes the whole `Schedules` map. The latter panics here in debug
    /// builds: `Schedule::remove_systems_in_set`'s own `initialize` call
    /// touches the `World`'s schedule bookkeeping in a way that re-inserts a
    /// `Schedules` resource before the scope closes, which `resource_scope`
    /// treats as a logic error (a schedule *entry* reappearing is expected
    /// and is exactly what `try_schedule_scope` is for; the whole *map*
    /// reappearing is not).
    pub(crate) fn detach(world: &mut World) -> usize {
        remove_from(world, Update) + remove_from(world, GameTick) + remove_from(world, Extract)
    }

    fn remove_from(world: &mut World, label: impl ScheduleLabel) -> usize {
        match world.try_schedule_scope(label, |world, schedule| {
            schedule.remove_systems_in_set(
                PresentationSet,
                world,
                ScheduleCleanupPolicy::RemoveSystemsOnly,
            )
        }) {
            Ok(Ok(n)) => n,
            // The schedule exists but never held the set.
            Ok(Err(ScheduleError::SetNotFound)) => 0,
            Ok(Err(e)) => panic!("failed to detach presentation systems: {e}"),
            // The schedule itself was never created — nothing to remove.
            Err(_) => 0,
        }
    }

    /// Re-add every presentation system, exactly as
    /// [`crate::sim::Sim::client_app`] first added it — see the module doc for
    /// why this goes through each plugin's own `add_presentation_systems`
    /// rather than through `Plugin::build`/`add_plugins`.
    ///
    /// **Only call this after [`detach`]** (or on a session whose presentation
    /// was never attached in the first place). Calling it twice in a row
    /// duplicates every presentation system — `Sim::attach_presentation`'s own
    /// `presentation_attached` guard is what makes the public entry point
    /// safe against that.
    pub(crate) fn attach(world: &mut World) {
        crate::entities::add_presentation_systems(world);
        crate::display_entities::add_presentation_systems(world);
        crate::mesher::add_presentation_systems(world);
        crate::interact::add_presentation_systems(world);
    }
}

#[cfg(feature = "runtime-presentation")]
pub(crate) use runtime::{attach, detach};
