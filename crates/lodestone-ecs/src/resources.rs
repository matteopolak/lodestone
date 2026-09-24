//! Core ECS resources — state that has exactly one owner in the `World`,
//! never a component on some entity.

use bevy_ecs::resource::Resource;
use lodestone_data::block_states::StateId;

/// The server's authoritative clock, folded from `ClientEvent::TimeChanged`.
///
/// Stage 0 of `docs/bevy-migration.md` moves this off
/// `lodestone_client::state::Inner` — where it was one of the two duplicate
/// state copies named in that doc's §1.1 — into a resource. The day/night
/// driver of sky and entity light
/// (`lodestone_render::entity::sky_darken_for_time_of_day`, wired in
/// `lodestone-shell`'s `app.rs`) reads it through
/// `lodestone_client::ClientHandle::world_time()`, which this is now the sole
/// backing store for: `Inner.world_age` / `Inner.time_of_day` no longer
/// exist, so there is nowhere left for a second copy to live.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorldTime {
    /// Total ticks since the world was created
    /// (`ClientEvent::TimeChanged::world_age`). Monotonically increasing,
    /// unlike `time_of_day`, which the server may freeze (`/gamerule
    /// doDaylightCycle false`) or set arbitrarily (`/time set`).
    pub age: i64,
    /// Ticks within the current day. The server does not wrap this to
    /// `0..24000` before sending it — `sky_darken_for_time_of_day` does that
    /// reduction itself — so treat it as an unbounded counter, not an angle.
    pub time_of_day: i64,
}

/// Vanilla's fixed tick period: 20 Hz, `1.0 / 20.0` seconds.
///
/// `f64` on purpose. The `f32` literal `0.05` is `0.050000000745…`, a relative
/// error of ~1.5e-8 against this value — about one tick of drift per 39 days of
/// continuous play. That term was measured and is *not* why the two pre-§4.1(c)
/// clocks diverged (the clamp was, see [`MAX_CATCH_UP_SECS`]), but there is no
/// reason to keep it now that there is one clock.
pub const TICK_PERIOD: f64 = 1.0 / 20.0;

/// Vanilla's own max-ticks-per-update constant: how many catch-up ticks
/// one driver iteration may run before the remaining backlog is dropped rather
/// than replayed in a burst.
pub const MAX_CATCH_UP_TICKS: u32 = 10;

/// [`MAX_CATCH_UP_TICKS`] expressed as seconds — the clamp
/// [`FrameClock::begin_frame`] applies to `dt`.
///
/// # This number is the §4.1(c) policy decision, and it is 0.5 not 0.25
///
/// Before the `World` unification there were **two** 20 Hz accumulators on two
/// different catch-up policies: `Sim::step` clamped `dt` to `0.25 s` (five
/// ticks) while `EntityInterpolator` banked the frame pacer's already-clamped
/// `0.5 s` (ten) unclamped. A maximal stall therefore advanced item physics five
/// ticks further than player physics, *per stall*, cumulatively and without
/// bound, because the excess real time was discarded rather than reconciled.
///
/// Unifying them forced a choice, and this is it: **ten ticks**, because
/// - it is vanilla's own `MAX_TICKS_PER_UPDATE`, which is the only external
///   oracle either candidate has;
/// - it is what `docs/frame-pacing.md` documents and what
///   `lodestone_shell::app::FramePacer` already clamps to, so the driver's own
///   two clamps now agree instead of one silently shadowing the other;
/// - the tighter `0.25 s` had no derivation. It predates the frame pacer and its
///   only written justification is the pacing test's own observation that it
///   binds first ("measured **5**, not 10"), i.e. a record of the discrepancy
///   rather than a reason for it.
///
/// The cost of loosening it is a longer worst-case catch-up burst: ten physics
/// ticks in one frame instead of five. That is vanilla's own worst case, and the
/// frame pacer already sized the budget for it.
pub const MAX_CATCH_UP_SECS: f64 = MAX_CATCH_UP_TICKS as f64 * TICK_PERIOD;

/// The **driver's** clock: real elapsed time, the fixed-timestep residual, and
/// the tick/frame counters derived from them.
///
/// Not the server's clock — that is [`WorldTime`], which the server sets and can
/// freeze. This one only ever moves forward with wall time and exists because
/// several consumers need "how long ago did this happen" in the *client's* own
/// frame of reference: the chat fade-out
/// (`lodestone_game::chat::ChatLog::recent_ages`), the render-side interpolation
/// factor, and the fixed-timestep health readout (`frames / ticks`).
///
/// # Why this is a resource and not four fields on the driver
///
/// `docs/bevy-migration.md` Stage 5. Stage 3 deferred `chat_log` explicitly
/// *because* of this: every chat push needs the clock and every read needs it
/// again to age the line, so moving the log to a component while the clock stayed
/// a `Sim` field would have put a second clock in the process. They move
/// together, and this is the half that had nowhere to live.
///
/// # `secs` is monotonic, `accumulator` is not
///
/// `secs` is the sum of every `dt` ever handed to the driver — it never
/// decreases and is never reset by a session teardown, which is what makes a
/// chat timestamp from before a reconnect still age correctly. `accumulator` is
/// the sub-tick residual and cycles within `[0, tick_period)`.
///
/// # This is the process's only 20 Hz accumulator (§4.1(c))
///
/// It used to be one of two: `lodestone_shell::entities::EntityInterpolator` had
/// its own `TickAccum` because it had its own `World`, and
/// `World::run_schedule(GameTick)` runs the systems in *that* `World`. One
/// `World` is therefore what makes one clock possible, and
/// [`begin_frame`](Self::begin_frame) / [`take_tick`](Self::take_tick) /
/// [`end_frame`](Self::end_frame) exist so the loop that drains it is written
/// once rather than per driver.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Default)]
pub struct FrameClock {
    /// Monotonic wall-clock seconds since the driver started, accumulated from
    /// the real per-frame `dt`.
    pub secs: f64,
    /// Seconds banked toward the next fixed tick. Always less than one tick
    /// period once the driver's tick loop has drained it.
    pub accumulator: f64,
    /// Fractional progress `[0,1)` from the last tick toward the next — the
    /// render-side interpolation factor.
    pub interp_alpha: f32,
    /// Total fixed ticks run since the driver started.
    pub ticks: u64,
    /// Total driver iterations (frames) since the driver started.
    pub frames: u64,
}

impl FrameClock {
    /// Start a driver iteration: advance monotonic time by the real `dt` and bank
    /// the catch-up-clamped share of it toward fixed ticks.
    ///
    /// `secs` takes the **unclamped** `dt` and `accumulator` the clamped one, and
    /// that asymmetry is deliberate: `secs` answers "how long ago did this chat
    /// line arrive", which must track wall time across a stall, while the
    /// accumulator answers "how many ticks do we owe", which must not replay a
    /// minute of them (see [`MAX_CATCH_UP_SECS`]).
    pub fn begin_frame(&mut self, dt: f64) {
        self.secs += dt.max(0.0);
        self.accumulator += dt.clamp(0.0, MAX_CATCH_UP_SECS);
        self.frames += 1;
    }

    /// Claim one fixed tick if the accumulator holds a whole period, counting it.
    ///
    /// The driver's loop is `while clock.take_tick() { … }`. Terminates because
    /// [`begin_frame`](Self::begin_frame) banks at most [`MAX_CATCH_UP_SECS`] and
    /// each claim withdraws a whole [`TICK_PERIOD`].
    pub fn take_tick(&mut self) -> bool {
        if self.accumulator >= TICK_PERIOD {
            self.accumulator -= TICK_PERIOD;
            self.ticks += 1;
            true
        } else {
            false
        }
    }

    /// Finish a driver iteration: publish the sub-tick residual as the render
    /// interpolation factor.
    ///
    /// Must run after the tick loop and before anything that interpolates — both
    /// the camera's between-tick ease and the entity walk cycle's partial tick
    /// read this, and they were two different residuals before the unification.
    pub fn end_frame(&mut self) {
        self.interp_alpha = (self.accumulator / TICK_PERIOD) as f32;
    }

    /// Drop the banked sub-tick residual (and the render factor derived from it)
    /// without touching monotonic time or the counters.
    ///
    /// For a session teardown. `Sim::end_session` used to reset the
    /// interpolator's accumulator (by replacing the whole interpolator) and *not*
    /// the player's, so a quit-to-title re-phased the two clocks arbitrarily on
    /// top of the clamp divergence. With one accumulator there is one thing to
    /// reset, and this is it.
    pub fn reset_accumulator(&mut self) {
        self.accumulator = 0.0;
        self.interp_alpha = 0.0;
    }

    /// Frames per fixed tick since start — the fixed-timestep health number the
    /// debug overlay draws. `0.0` before the first tick, rather than a division
    /// by zero.
    #[must_use]
    pub fn frames_per_tick(&self) -> f32 {
        if self.ticks == 0 {
            0.0
        } else {
            self.frames as f32 / self.ticks as f32
        }
    }
}

/// The version adapter for the configured protocol, as a resource — §4.3 of
/// `docs/bevy-migration.md`.
///
/// Kept a trait object deliberately: a generic parameter would monomorphise the
/// whole `App` per protocol family and force whoever builds it to *name* a
/// version, which is the thing `lodestone-shell` has never done (its only route
/// to version data is `lodestone_registry::adapter_for_protocol`). `VersionAdapter`
/// is already declared `Send + Sync + Debug`, so this needs no signature change
/// anywhere.
///
/// `None` is a real, expected state and not an error: it means **no version
/// family is compiled in** for that protocol, which is every build without
/// `--features live`. Consumers must degrade honestly rather than substituting a
/// default — the mining predictor, for instance, refuses to dig rather than
/// guessing a hardness, because guessing one is precisely how block breaking got
/// too fast the first time.
#[derive(Resource, Debug, Default)]
pub struct VersionData(pub Option<Box<dyn lodestone_model::VersionAdapter>>);

impl VersionData {
    /// The version's break-time census for a block-state id, or `None` when
    /// there is no adapter or the id is outside its census. The two causes are
    /// deliberately not distinguished: the correct response to both is the same,
    /// refuse to dig.
    #[must_use]
    pub fn block_hardness(&self, state_id: StateId) -> Option<lodestone_model::BlockHardness> {
        self.0.as_ref()?.block_hardness(state_id.raw())
    }

    /// The held item's mining contribution for a block-state id, or `None` when
    /// there is no adapter or nothing in the census.
    #[must_use]
    pub fn tool_mining(
        &self,
        held: Option<&lodestone_model::ItemStack>,
        state_id: StateId,
    ) -> Option<lodestone_model::ToolMining> {
        self.0.as_ref()?.tool_mining(held, state_id.raw())
    }

    /// The version's pick/outline geometry for a block-state id, block-local
    /// `0..1`-per-cell boxes, or `None` when there is no adapter or the id is
    /// outside its census.
    ///
    /// Added for [`crate::player::BreakIntent`]'s legality check: a plugin
    /// supplies a target with no mouse ray behind it, so `drive_mining` has to
    /// cast one of its own from the eye to confirm the target is actually
    /// reachable and unobstructed — the same question a human's crosshair
    /// answers for free. This is the read that lets it build the same
    /// `pick_boxes` closure a live mouse cast already uses.
    #[must_use]
    pub fn block_outline(&self, state_id: StateId) -> Option<&'static [lodestone_model::BlockAabb]> {
        self.0.as_ref()?.block_outline(state_id.raw())
    }

    /// The version's per-entity-type physics facts for a resolved entity type, or
    /// `None` when there is no adapter or the type is outside its census.
    ///
    /// Keyed by [`lodestone_model::ResourceKey`] because that is the only entity
    /// identity that survives ingest — `ClientEvent::EntitySpawned` resolves the
    /// `add_entity` varint away and stores the key in `EntityKind`, so a physics
    /// consumer has no wire id to hand [`lodestone_model::VersionAdapter::entity_dimensions`].
    ///
    /// As with [`Self::block_hardness`], the two `None` causes are deliberately
    /// not distinguished: the correct response to both is the same, and for the
    /// entity-push producer that response is **treat it as not a pusher**. See
    /// [`lodestone_model::EntityFacts::pushes_players`] on why the default has to
    /// be deny.
    #[must_use]
    pub fn entity_facts(
        &self,
        entity_type: &lodestone_model::ResourceKey,
    ) -> Option<lodestone_model::EntityFacts> {
        self.0.as_ref()?.entity_facts(entity_type)
    }

    /// Whether an entity of this type can shove the local player — the
    /// [`Self::entity_facts`] field the push producer gates on, with the
    /// default-deny already applied so no caller can forget it.
    ///
    /// `false` for an unknown type and for a build with no version family
    /// compiled in, which is the honest answer in both cases: nothing about an
    /// unrecognised entity licenses letting it move the player.
    #[must_use]
    pub fn entity_pushes_players(&self, entity_type: &lodestone_model::ResourceKey) -> bool {
        self.entity_facts(entity_type)
            .is_some_and(|facts| facts.pushes_players)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_push_defaults_to_deny_with_no_version_family_compiled_in() {
        // A build without `--features live` has `VersionData(None)`. Every entity
        // must then be reported as *not* a pusher — the honest degradation, and the
        // one that cannot make a dropped item shove the player.
        let version = VersionData::default();
        for name in ["minecraft:zombie", "minecraft:item", "someplugin:custom"] {
            let key: lodestone_model::ResourceKey = name.parse().expect("parses");
            assert!(version.entity_facts(&key).is_none());
            assert!(
                !version.entity_pushes_players(&key),
                "{name} must not push with no adapter"
            );
        }
    }

    #[test]
    fn the_push_producer_borrow_shape_holds() {
        // Pins the borrow pattern `lodestone-shell`'s `Sim::tick_nearby_entities`
        // uses: build the `QueryState` from `&mut World` (which ends that mutable
        // borrow), then hold `&VersionData` and iterate the query *simultaneously*,
        // both as immutable reborrows. Reading the resource before the loop is what
        // avoids a second `hold_write` pass or a per-entity resource lookup, and it
        // only compiles because neither borrow is mutable — so it is worth a test
        // rather than a comment.
        use crate::entity::{EntityKind, Position};
        use lodestone_model::Vec3;

        let mut world = bevy_ecs::world::World::new();
        world.insert_resource(VersionData::default());
        world.spawn((
            Position(Vec3 {
                x: 1.0,
                y: 2.0,
                z: 3.0,
            }),
            EntityKind("minecraft:zombie".parse().expect("parses")),
        ));

        // `&mut World` is what `lodestone_ecs::hold_write` hands its closure, so
        // the reborrow is exercised through the same shape the shell has.
        let admitted = (|w: &mut bevy_ecs::world::World| {
            let mut state = w.query::<(&Position, &EntityKind)>();
            let version = w.resource::<VersionData>();
            state
                .iter(w)
                .filter(|(_, kind)| version.entity_pushes_players(&kind.0))
                .count()
        })(&mut world);
        // No adapter, so nothing is admitted — the assertion that matters here is
        // that this compiles at all, but a count keeps the test non-vacuous.
        assert_eq!(admitted, 0);
    }

    /// Drain one frame's worth of `dt` and report how many fixed ticks it bought.
    fn ticks_in_one_frame(dt: f64) -> u32 {
        let mut clock = FrameClock::default();
        clock.begin_frame(dt);
        let mut ticks = 0;
        while clock.take_tick() {
            ticks += 1;
        }
        clock.end_frame();
        ticks
    }

    /// The §4.1(c) policy, pinned: a stall long enough to exhaust the budget runs
    /// **ten** ticks, not five.
    ///
    /// Five was the number `lodestone_shell::app`'s pacing test measured before
    /// the unification, because `Sim::step` applied a second, tighter `0.25 s`
    /// clamp on top of the pacer's `0.5 s`. There is now one clamp.
    #[test]
    fn a_stall_longer_than_the_budget_runs_exactly_ten_ticks() {
        assert_eq!(MAX_CATCH_UP_TICKS, 10);
        assert_eq!(ticks_in_one_frame(60.0), MAX_CATCH_UP_TICKS);
        assert_eq!(ticks_in_one_frame(MAX_CATCH_UP_SECS), MAX_CATCH_UP_TICKS);
    }

    /// The control for the above: a *short* frame is not clamped, so the
    /// assertion is measuring the clamp rather than a constant ceiling every
    /// input would hit.
    #[test]
    fn a_frame_shorter_than_the_budget_is_not_clamped() {
        assert_eq!(ticks_in_one_frame(0.0), 0);
        assert_eq!(ticks_in_one_frame(TICK_PERIOD), 1);
        assert_eq!(ticks_in_one_frame(3.0 * TICK_PERIOD), 3);
    }

    /// Monotonic time tracks the *unclamped* stall even though the tick budget
    /// does not — otherwise a chat line stamped before a one-minute stall would
    /// age by half a second across it.
    #[test]
    fn monotonic_seconds_are_not_clamped_with_the_accumulator() {
        let mut clock = FrameClock::default();
        clock.begin_frame(60.0);
        assert!((clock.secs - 60.0).abs() < 1e-12);
        assert!(clock.accumulator <= MAX_CATCH_UP_SECS);
    }

    /// The residual published for interpolation is the *post-loop* one: half a
    /// tick of leftover reads as `0.5`, not as the whole frame's fraction.
    #[test]
    fn the_interpolation_factor_is_the_residual_after_the_loop() {
        let mut clock = FrameClock::default();
        clock.begin_frame(TICK_PERIOD * 1.5);
        while clock.take_tick() {}
        clock.end_frame();
        assert_eq!(clock.ticks, 1);
        assert!(
            (clock.interp_alpha - 0.5).abs() < 1e-6,
            "{}",
            clock.interp_alpha
        );
    }

    /// A teardown drops the sub-tick residual but never rewinds monotonic time —
    /// the asymmetry `reset_accumulator` exists for.
    #[test]
    fn resetting_the_accumulator_leaves_monotonic_time_alone() {
        let mut clock = FrameClock::default();
        clock.begin_frame(TICK_PERIOD * 1.5);
        while clock.take_tick() {}
        clock.end_frame();
        clock.reset_accumulator();
        assert_eq!(clock.accumulator, 0.0);
        assert_eq!(clock.interp_alpha, 0.0);
        assert!(clock.secs > 0.0);
        assert_eq!(clock.ticks, 1);
    }
}
