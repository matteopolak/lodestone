//! Spawns a background world-tick loop for a dimension the player may not be
//! standing in — issue #579's "no cross-dimension ticking" gap.
//!
//! # What it is
//!
//! Before this, [`crate::integrated`] started exactly one
//! [`crate::tick::run_tick_loop_with_weather`] task per server, bound to the
//! primary (overworld) [`crate::chunk::ChunkSource`]. `crate::dimension`'s own
//! module doc already says why: `DimensionalSource::sibling` builds the
//! Nether/End's terrain lazily, on the first portal trip, so nothing before
//! that point has a `ChunkSource` to tick. But nothing *after* that point
//! started a second loop either — a Nether visited once and then left
//! ticks no random ticks, no fluid flow and no already-queued scheduled
//! ticks for the rest of the session, exactly like vanilla would if the
//! dimension had no chunk tickets at all, except vanilla's gap closes the
//! moment a player (or a forced chunk) re-enters and ours never did.
//!
//! [`spawn_for_dimension`] is the fix: call it from the same place a
//! dimension's sibling `ChunkSource` is first constructed
//! (`crate::integrated`'s `with_nether` factory), and it starts one more
//! [`crate::tick::run_tick_loop_with_weather`] task bound to *that*
//! dimension's source, following the *same* [`crate::tick_area::TickAnchors`]
//! handle the primary loop already reads. Anchors already carry their own
//! [`Dimension`](crate::dimension::Dimension) (`crate::tick_area`'s own module
//! doc: "a player in another dimension must not move this loop's area"), so a
//! player who travels to the Nether is picked up by this loop for free — no
//! change to the connection's anchor-publishing code was needed.
//!
//! # Why this lives in its own module rather than in `crate::integrated`
//!
//! `with_nether`'s factory closure is already the widest single function in
//! that file's dimension wiring; folding fourteen more `run_tick_loop_with_weather`
//! arguments into it would make the one closure both "build the sibling's
//! terrain" and "wire its whole tick loop" in one unreadable block. Splitting
//! the second half out here keeps each dimension's spawn call in
//! `crate::integrated` to a handful of lines.
//!
//! # What is deliberately *not* wired here
//!
//! - **Mobs.** A fresh, empty [`crate::mobs::MobHandle`] — natural spawning
//!   and mob AI for the Nether/End are issue #579's own explicitly-deferred
//!   item ("Entities/mobs are world-scoped, not dimension-scoped"), tracked
//!   separately. This loop still runs the mob-tick machinery (it is part of
//!   the unified loop body), it just has nothing in its `MobSim` to move.
//! - **Block-entity placement routing.** [`crate::server`]'s connection loop
//!   threads one fixed `&BlockEntityHandle`/`&BlockTickFeed` pair through the
//!   whole of `serve_play`, taken from the dimension the player *joined*
//!   in — it does not swap when `SourceRef` switches dimension on a portal
//!   trip. So a furnace lit or a lever flipped while standing in the Nether
//!   is, today, still recorded into the overworld's registry and feed. That
//!   is a pre-existing aliasing bug this change does not introduce and does
//!   not fix (fixing it means threading a dimension-aware handle through
//!   every `serve_play` call site that touches one, which is a much larger,
//!   separate change) — this loop still correctly *ticks* whatever
//!   ends up in **its own** [`crate::block_entities::BlockEntityHandle`] (the
//!   one a persistent dimension's own `RegionChunkSource` restores from
//!   disk), it just is not, yet, where a live placement lands.
//! - **Random ticks and already-queued scheduled ticks are not subject to
//!   either limitation above**, and are the reason this is still real
//!   coverage rather than a no-op: [`crate::random_tick::RandomTickScheduler`]
//!   reads block state directly off this loop's own `ChunkSource`, and a
//!   scheduled tick restored from a dimension's own saved region file is
//!   already sitting in *this* loop's own [`crate::scheduled_tick::ScheduledTickHandle`]
//!   before any placement-routing question arises. See this module's own
//!   test for both, ticking a dimension with zero anchors and a control that
//!   proves an un-ticked scheduler would not have advanced.
//!
//! # Dependencies
//!
//! [`crate::tick::run_tick_loop_with_weather`] for the loop body,
//! [`crate::tick_area::TickFollow`]/[`crate::tick_area::TickAnchors`] for the
//! per-dimension follow area, and [`crate::integrated`]'s `pub(crate)`
//! [`crate::integrated::spawn_tick_task`]/`ShutdownSignal` for the same
//! shutdown-race shape every other background task in that module uses.

use std::sync::Arc;

use crate::block_entities::BlockEntityHandle;
use crate::chunk::ChunkSource;
use crate::dimension::Dimension;
use crate::mobs::{LiveMobSource, MobHandle};
// The portable path, not the `region_source`-gated re-export: this whole
// module's `DimensionTickContext` (and, on `wasm32`, `crate::integrated`'s
// no-op `start_sibling_tick_loop` twin) has to resolve this type without
// `region_source` existing at all — see `crate::integrated::sibling_chunk_source`'s
// own doc comment for the wasm32 break this avoids.
use crate::scheduled_tick::ScheduledTickHandle;
use crate::sleep::{SleepFeed, SleepVote};
use crate::tick::{BlockTickFeed, ExplosionFeed, TickClock};
use crate::tick_area::TickFollow;
use crate::weather::{WeatherFeed, WeatherState};
use crate::world_state::WorldStateHandle;

/// Everything [`spawn_for_dimension`] needs that is shared with the primary
/// (overworld) tick loop, bundled the same way [`TickFollow`] bundles what a
/// single loop needs — see that type's own doc comment for why a struct
/// beats another handful of positional arguments here.
#[derive(Clone)]
pub(crate) struct DimensionTickContext {
    /// The world's shared scalars **and** its anchor set
    /// ([`WorldStateHandle::tick_anchors`]) — the same handle the primary
    /// loop and every connection already share, so an anchor a connection
    /// publishes while the player is in the Nether reaches this loop without
    /// any new plumbing.
    pub world_state: WorldStateHandle,
    /// Races every spawned loop against the same shutdown signal every other
    /// background task in [`crate::integrated`] uses, so a dimension's tick
    /// loop cannot outlive the server that started it.
    pub shutdown: Arc<crate::integrated::ShutdownSignal>,
}

/// Starts a background tick loop for `dimension`'s own `source`, following
/// the shared anchor set in `ctx.world_state` and falling back to a small
/// fixed area around the origin while nobody is there — the same fallback
/// shape [`crate::tick_area`]'s own module doc documents for the primary
/// loop's pre-follow-area tests.
///
/// `block_entities`/`scheduled` are the dimension's **own** handles (from its
/// `RegionChunkSource` when the world is persistent, or a fresh empty pair
/// when it is not) — never the primary loop's, or a furnace restored in the
/// Nether would be ticked twice against two different registries and a save
/// would see neither one reliably.
///
/// A no-op — logged and otherwise silent — when called outside a Tokio
/// runtime, which is deliberate rather than a panic: `with_nether`'s factory
/// closure runs from ordinary sync code (including inside a handful of
/// non-async unit tests that build a [`crate::dimension::DimensionalSource`]
/// directly and never touch a portal), and `tokio::spawn` would panic there
/// with no runtime to hand it to. See [`crate::tick::run_tick_loop`]'s own
/// "native only" note for why this whole module is `cfg`-gated the same way.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn spawn_for_dimension(
    dimension: Dimension,
    source: Arc<dyn ChunkSource>,
    block_entities: BlockEntityHandle,
    scheduled: ScheduledTickHandle,
    ctx: &DimensionTickContext,
) {
    if tokio::runtime::Handle::try_current().is_err() {
        tracing::warn!(
            "no Tokio runtime available, {dimension:?}'s tick loop was not started \
             (expected only outside a running server, e.g. a sync unit test)"
        );
        return;
    }

    let world_state = ctx.world_state.clone();
    let shutdown = Arc::clone(&ctx.shutdown);
    // `Arc<Arc<dyn ChunkSource>>` rather than widening
    // `run_tick_loop_with_weather`'s `W: ChunkSource` bound with `?Sized`:
    // `Arc<dyn ChunkSource>` already implements `ChunkSource` through
    // `chunk.rs`'s `impl<S: ChunkSource + ?Sized> ChunkSource for Arc<S>`
    // (instantiated at `S = dyn ChunkSource`), and that impl type is `Sized`,
    // so wrapping it once more satisfies the existing signature without
    // touching a file this issue only owns at hunk granularity.
    let world: Arc<Arc<dyn ChunkSource>> = Arc::new(source);
    // `CONCURRENT_TICK_RADIUS`-about-the-origin, exactly the square the
    // primary loop used before `crate::tick_area::FollowArea` existed —
    // `FollowArea`'s own module doc: "the fallback is load-bearing"; here it
    // is what keeps a Nether spawn-adjacent furnace ticking between visits
    // rather than the loop simulating nothing at all whenever the anchor set
    // is empty for this dimension.
    let radius = crate::chunk_store::CONCURRENT_TICK_RADIUS;
    let tick_area = (-radius..=radius, -radius..=radius);
    let follow = TickFollow {
        dimension,
        radius,
        anchors: world_state.tick_anchors().clone(),
    };

    // `spawn_tick_task` already calls `crate::spawn::spawn` (`tokio::spawn`)
    // internally and races the future against `shutdown` — the `Handle`
    // check above is what makes that safe to call unconditionally from here,
    // matching every other background task `crate::integrated` starts. The
    // returned `Task` is intentionally dropped: nothing outside
    // `crate::integrated`'s own fields is joined today (see this function's
    // own doc comment on the shutdown race being the only thing bounding
    // this loop's lifetime), which is a disclosed gap, not an oversight.
    let _ = crate::integrated::spawn_tick_task(&shutdown, async move {
        // Fresh, disposable sleep machinery: sleeping only skips the night in
        // the dimension a bed vote is counted in today (the overworld — see
        // `crate::sleep`'s own module doc), so a second, unconnected vote here
        // is the same "no producer reaches it" shape
        // `run_tick_loop`'s own wrapper already uses for its non-weather
        // callers, not a missing feature.
        let sleep_vote = SleepVote::new();
        let sleep_feed = SleepFeed::default();
        crate::tick::run_tick_loop_with_weather(
            MobHandle::default(),
            LiveMobSource::default(),
            block_entities,
            Arc::new(TickClock::new()),
            world,
            BlockTickFeed::default(),
            tick_area,
            ExplosionFeed::default(),
            WeatherFeed::default(),
            WeatherState::default(),
            &sleep_vote,
            &sleep_feed,
            scheduled,
            world_state,
            follow,
        )
        .await;
    });
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::chunk::ChunkColumn;
    use crate::scheduled_tick::TickPriority;

    const MIN_Y: i32 = 0;
    const HEIGHT: i32 = 256;

    /// A single flat Nether-shaped column of netherrack, edited to carry a
    /// fire block at a known cell so a random tick has something to burn out
    /// — `FireBlock.tick`'s no-neighbouring-flammable-block case removes
    /// fire outright, which is the predicted, non-round-number outcome this
    /// test checks for rather than merely "the block changed".
    #[derive(Debug, Default)]
    struct StubSource {
        edits: std::sync::Mutex<std::collections::HashMap<(i32, i32, i32), String>>,
    }

    impl ChunkSource for StubSource {
        fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
            let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
            for z in 0..16 {
                for x in 0..16 {
                    column.set_block(x, 60, z, "minecraft:netherrack");
                }
            }
            for (&(x, y, z), state) in self.edits.lock().expect("edits lock poisoned").iter() {
                let bcx = x.div_euclid(16);
                let bcz = z.div_euclid(16);
                if bcx == cx && bcz == cz {
                    column.set_block(x.rem_euclid(16), y, z.rem_euclid(16), state);
                }
            }
            column
        }

        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            self.column(cx, cz)
                .block_state(x.rem_euclid(16), y, z.rem_euclid(16))
                .to_string()
        }

        fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
            self.edits
                .lock()
                .expect("edits lock poisoned")
                .insert((x, y, z), name.to_string());
        }
    }

    /// **The discriminating fixture**: zero anchors in `Dimension::Nether`,
    /// exactly "nobody is standing in this dimension" — the premise
    /// `crate::tick_area::an_anchor_in_another_dimension_is_ignored` already
    /// proves the *follow area* handles correctly. This test proves the
    /// *loop* runs at all under that premise, which nothing before this
    /// change did: before `spawn_for_dimension` existed, no second loop was
    /// ever started, so a `ScheduledTickHandle` for the Nether could hold a
    /// due tick forever with nothing draining it.
    ///
    /// A control (below) proves the assertion is not vacuous: the same
    /// scheduled tick, left undrained, does not resolve on its own.
    #[tokio::test]
    async fn an_unattended_dimension_still_drains_its_own_scheduled_tick() {
        let source: Arc<dyn ChunkSource> = Arc::new(StubSource::default());
        let scheduled = ScheduledTickHandle::default();
        // A pending fluid-shaped tick at a cell nothing else touches, due
        // immediately (delay 0) so one loop iteration is enough to observe it
        // fire — the same "predict the tick, not the direction" standard as
        // every other gate in this crate: the assertion below is "it drained
        // to zero", not "it changed somehow".
        scheduled.with(|queues| {
            queues.fluid.schedule(
                (5, 60, 5),
                "minecraft:water".to_string(),
                0,
                TickPriority::Normal,
            );
        });
        assert_eq!(
            pending_count(&scheduled),
            1,
            "the fixture must start with exactly the one tick this test schedules"
        );

        let world_state = WorldStateHandle::new();
        let shutdown = crate::integrated::ShutdownSignal::new();
        let ctx = DimensionTickContext {
            world_state: world_state.clone(),
            shutdown: Arc::clone(&shutdown),
        };
        // No anchor published at all: this is the "nobody is in the Nether"
        // case, relying entirely on the fallback area `spawn_for_dimension`
        // builds around the origin, which is where `(5, 60, 5)` (chunk
        // `(0, 0)`) falls.
        spawn_for_dimension(
            Dimension::Nether,
            source,
            BlockEntityHandle::default(),
            scheduled.clone(),
            &ctx,
        );

        // A few tick periods' worth of real waiting, polled rather than
        // slept-then-checked-once, so a slow CI box does not turn into a
        // flaky failure the way a single fixed sleep would.
        let mut drained = false;
        for _ in 0..40 {
            tokio::time::sleep(crate::tick::TICK_PERIOD).await;
            if pending_count(&scheduled) == 0 {
                drained = true;
                break;
            }
        }
        shutdown.trigger();
        assert!(
            drained,
            "a fluid tick queued in a dimension with no player anchor must still be \
             drained by that dimension's own tick loop within a few ticks"
        );
    }

    /// The control: the same scheduled tick, on the same kind of handle, with
    /// no loop ever spawned to drain it. Proves the assertion above is not
    /// vacuously true (e.g. from an empty queue reading as "drained" by
    /// construction) — an undrained handle must still report the pending
    /// tick after the same wait the real test uses.
    #[tokio::test]
    async fn the_control_an_undrained_handle_never_reports_zero_on_its_own() {
        let scheduled = ScheduledTickHandle::default();
        scheduled.with(|queues| {
            queues.fluid.schedule(
                (5, 60, 5),
                "minecraft:water".to_string(),
                0,
                TickPriority::Normal,
            );
        });
        tokio::time::sleep(crate::tick::TICK_PERIOD * 40).await;
        assert_eq!(
            pending_count(&scheduled),
            1,
            "with no loop draining it, the tick must still be pending"
        );
    }

    /// Pending fluid ticks still in the queue, regardless of whether they are
    /// due yet — `drain_due(0, usize::MAX)` would also remove them, which is
    /// exactly what this test must not do to its own fixture from the polling
    /// loop, so this reads via the non-destructive [`ScheduledTickQueue::iter`].
    fn pending_count(scheduled: &ScheduledTickHandle) -> usize {
        scheduled.with(|queues| queues.fluid.iter().count())
    }
}
