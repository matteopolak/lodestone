//! The dimension-filtered columns a world tick loop follows.
//!
//! [`TickAnchors`] derives every connected player's chunk position from the
//! canonical registry. [`FollowArea::recompute`] unions the followed squares
//! for players in this loop's dimension and produces a [`TickRegionPlan`].
//! With no applicable player it uses the caller's fixed fallback square.
//!
//! Coordinate planning is arithmetic only. [`FollowArea::snapshot_terrain_if_resident`]
//! copies available columns and defers a missing footprint. Terrain admission
//! remains owned by connection and source workflows. The natural spawner can
//! therefore defer a cold footprint while the tick loop continues handling
//! resident work.
//!
//! [`TickAnchors::publish`] provides manual fixture anchors when no registered
//! player is present. Production connections update registry presence, and a
//! dropped player ticket automatically withdraws its anchor. Each dimension
//! ignores players in other dimensions.
//!
//! [`TickFollow::radius`] defaults to
//! [`crate::chunk_store::CONCURRENT_TICK_RADIUS`]. Changing it also requires
//! checking residency capacity and the natural-spawn census geometry.

use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};

use crate::chunk::ChunkSource;
use crate::dimension::Dimension;
use crate::mobs::ChunkWorld;
use crate::tick_region::{CandidateRegionWorkload, TickOwnedChunk, TickRegionPlan};

/// The spawn radius in chunks: a chunk within this Chebyshev distance of a
/// player counts toward the global spawn caps.
pub const SPAWN_DISTANCE_CHUNKS: i32 = 8;

/// One player's position as the world tick loop needs it: which dimension they
/// are in and which chunk column they are standing in.
///
/// Chunk coordinates rather than a `Vec3` because that is all the area needs, and
/// carrying the full position would invite a consumer to re-derive
/// `floor(block / 16)` — an arithmetic right shift, not a truncating divide, which
/// this crate has already got wrong once for negative coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickAnchor {
    /// The dimension this player is in. A [`FollowArea`] for a different one
    /// ignores the anchor entirely.
    pub dimension: Dimension,
    /// The player's chunk X.
    pub cx: i32,
    /// The player's chunk Z.
    pub cz: i32,
}

/// Per-tick player anchors, backed by the canonical registry in production.
#[derive(Debug, Clone, Default)]
pub struct TickAnchors {
    manual: Arc<Mutex<Vec<TickAnchor>>>,
    players: Option<crate::players::PlayerRegistry>,
}

impl TickAnchors {
    pub(crate) fn from_players(players: crate::players::PlayerRegistry) -> Self {
        Self { players: Some(players), ..Self::default() }
    }

    /// Replaces the whole anchor set.
    ///
    /// This compatibility input is used only when the linked registry is empty.
    pub fn publish(&self, anchors: Vec<TickAnchor>) {
        if let Ok(mut guard) = self.manual.lock() {
            *guard = anchors;
        }
    }

    /// The current anchor set.
    ///
    /// A poisoned lock reads as **empty**, which lands on the fallback area rather
    /// than on a panic inside the world tick: a tick loop that dies takes the
    /// world with it, and the fallback is a strictly-correct-if-stale answer.
    #[must_use]
    pub fn snapshot(&self) -> Vec<TickAnchor> {
        if let Some(players) = &self.players {
            let anchors = players.tick_anchors();
            if !anchors.is_empty() { return anchors; }
        }
        self.manual.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

/// Everything [`crate::tick::run_tick_loop`] needs in order to follow the
/// players, in one parameter.
///
/// Bundled rather than three arguments because that loop already takes fourteen,
/// and because a caller that does not care wants exactly one
/// [`Default::default`] — which yields an empty anchor set and therefore the
/// caller's own fallback square, i.e. the behaviour before this existed.
#[derive(Debug, Clone)]
pub struct TickFollow {
    /// The dimension this loop's [`ChunkSource`] serves. Anchors from any other
    /// dimension are ignored.
    pub dimension: Dimension,
    /// The follow radius in columns, giving a `(2 * radius + 1)²` square per
    /// player. Defaults to [`crate::chunk_store::CONCURRENT_TICK_RADIUS`].
    pub radius: i32,
    /// Where player positions arrive from.
    pub anchors: TickAnchors,
}

impl Default for TickFollow {
    fn default() -> Self {
        Self {
            dimension: Dimension::Overworld,
            radius: crate::chunk_store::CONCURRENT_TICK_RADIUS,
            anchors: TickAnchors::default(),
        }
    }
}

/// The columns the world tick loop simulates this tick.
///
/// Rebuilt in place rather than reallocated, and [`recompute`](Self::recompute)
/// reports whether the set actually *moved* — which is the signal the expensive
/// half (the terrain view) is gated on.
#[derive(Debug)]
pub struct FollowArea {
    follow: TickFollow,
    /// The square used when no anchor names this loop's dimension. See the module
    /// doc for why this is not simply "nothing".
    fallback: Vec<(i32, i32)>,
    plan: TickRegionPlan,
    /// Chunks within [`SPAWN_DISTANCE_CHUNKS`] of any applicable player, the
    /// quantity the global spawn caps scale with. See
    /// [`Self::spawn_cap_chunks`].
    spawn_cap_chunks: i32,
    /// Scratch, reused across ticks so a per-tick recompute allocates nothing
    /// after the first.
    scratch: Vec<(i32, i32)>,
}

impl FollowArea {
    /// A follow area over `follow`, falling back to the inclusive
    /// `(cx_range, cz_range)` square when no anchor applies.
    ///
    /// The initial [`chunks`](Self::chunks) is the fallback, so a loop that reads
    /// it before its first [`recompute`](Self::recompute) sees the same area it
    /// always did.
    #[must_use]
    pub fn new(
        follow: TickFollow,
        cx_range: std::ops::RangeInclusive<i32>,
        cz_range: std::ops::RangeInclusive<i32>,
    ) -> Self {
        let fallback: Vec<(i32, i32)> = cz_range
            .flat_map(|cz| cx_range.clone().map(move |cx| (cx, cz)))
            .collect();
        Self {
            follow,
            spawn_cap_chunks: i32::try_from(fallback.len()).unwrap_or(i32::MAX),
            plan: TickRegionPlan::chunk_owned(fallback.clone()),
            fallback,
            scratch: Vec::new(),
        }
    }

    /// Recomputes the area from the current anchor set, returning `true` when the
    /// column set changed.
    ///
    /// The union over anchors, not the first one: two players in the same
    /// dimension both tick their surroundings, and overlapping squares must not
    /// tick a shared column twice (a doubled random tick is a doubled crop growth
    /// rate, which is a *behavioural* divergence rather than a waste).
    /// Sorted-then-deduped rather than a `HashSet` so the resulting order is
    /// stable — the random-tick stream's draw order depends on the visit order,
    /// and a set iteration order that varies per tick would make growth
    /// unreproducible.
    pub fn recompute(&mut self) -> bool {
        let anchors = self.follow.anchors.snapshot();
        self.scratch.clear();
        let r = self.follow.radius.max(0);
        for anchor in anchors.iter().filter(|a| a.dimension == self.follow.dimension) {
            for dz in -r..=r {
                for dx in -r..=r {
                    self.scratch
                        .push((anchor.cx.saturating_add(dx), anchor.cz.saturating_add(dz)));
                }
            }
        }
        let mut cap_chunks: Vec<(i32, i32)> = Vec::new();
        for anchor in anchors.iter().filter(|a| a.dimension == self.follow.dimension) {
            for dz in -SPAWN_DISTANCE_CHUNKS..=SPAWN_DISTANCE_CHUNKS {
                for dx in -SPAWN_DISTANCE_CHUNKS..=SPAWN_DISTANCE_CHUNKS {
                    cap_chunks.push((anchor.cx.saturating_add(dx), anchor.cz.saturating_add(dz)));
                }
            }
        }
        cap_chunks.sort_unstable();
        cap_chunks.dedup();
        self.spawn_cap_chunks = if cap_chunks.is_empty() {
            i32::try_from(self.fallback.len()).unwrap_or(i32::MAX)
        } else {
            i32::try_from(cap_chunks.len()).unwrap_or(i32::MAX)
        };
        if self.scratch.is_empty() {
            self.scratch.extend_from_slice(&self.fallback);
        } else {
            self.scratch.sort_unstable();
            self.scratch.dedup();
        }
        if self.scratch == self.plan.chunks() {
            return false;
        }
        self.plan = TickRegionPlan::chunk_owned(std::mem::take(&mut self.scratch));
        true
    }

    /// The columns to simulate this tick.
    #[must_use]
    pub fn chunks(&self) -> &[(i32, i32)] {
        self.plan.chunks()
    }

    /// The same selected chunks, assigned to the smallest region-local owner.
    ///
    /// The server still runs this sequence serially. Its order deliberately
    /// matches [`Self::chunks`] so changing the ownership boundary cannot shift
    /// random-number draws before a future hand-off design says it may.
    #[must_use]
    pub fn owned_chunks(&self) -> &[TickOwnedChunk] {
        self.plan.owned_chunks()
    }

    /// The chunk count the global spawn caps scale with: the union of the
    /// `(2 * 8 + 1)²` squares around every applicable player, whether or not
    /// those chunks are resident.
    ///
    /// The caps are `per-category maximum × count / 289`, so one player always
    /// yields the full per-category maxima. This is deliberately **not** the
    /// simulated follow area ([`Self::chunks`]): the server only streams and
    /// ticks a few columns around each player, and scaling the caps by that
    /// count would round the small categories (water creatures, axolotls,
    /// underground water creatures) down to zero so they never spawn.
    #[must_use]
    pub fn spawn_cap_chunks(&self) -> i32 {
        self.spawn_cap_chunks
    }

    /// Groups this live tick area's selected chunks into observer-chosen
    /// candidate regions without changing the current global owner.
    ///
    /// This is the measurement seam for a named populated scene. Callers must
    /// report the chosen edge with their result; it is not server configuration
    /// and does not start region workers.
    #[must_use]
    pub fn candidate_region_workload(
        &self,
        edge_chunks: NonZeroU32,
    ) -> CandidateRegionWorkload {
        self.plan.candidate_region_workload(edge_chunks)
    }

    /// Snapshots this area's terrain out of `source` into the view the natural
    /// spawner reads.
    ///
    /// **Call this only when [`recompute`](Self::recompute) returned `true`, or on
    /// a staleness cadence** — see the module doc's cost table. Every column here
    /// goes through `ChunkSource::column`, which is a cheap clone for a resident
    /// column and a full generator run for a cold one.
    ///
    /// `Arc` rather than the leaked `&'static` [`crate::MobHandle`] uses: this view
    /// is replaced as the player walks, so leaking one per boundary crossing would
    /// leak ~31 KiB per column for the life of the process. The spawner holds a
    /// clone and drops it when the next one arrives.
    #[must_use]
    pub fn snapshot_terrain<S: ChunkSource + ?Sized>(&self, source: &S) -> Arc<ChunkWorld> {
        Arc::new(ChunkWorld::from_columns(
            self.chunks()
                .iter()
                .map(|&(cx, cz)| ((cx, cz), source.column(cx, cz))),
        ))
    }

    /// Snapshots this area's terrain only when every selected column is already
    /// resident.
    ///
    /// The ordinary [`snapshot_terrain`](Self::snapshot_terrain) API is still
    /// useful to callers that explicitly want a complete, loading snapshot.
    /// The live tick loop cannot make that choice for a bounded source: a player
    /// anchor can arrive while the join stream is still generating the same
    /// columns, and a cold `ChunkSource::column` call would synchronously wait
    /// on that generation while the 20 Hz world clock is supposed to keep
    /// moving. Returning `None` leaves the caller to retry after the stream has
    /// populated the cache, without taking a write gate or starting generation
    /// on the tick task. Sources with no bounded cache retain the trait's
    /// `is_column_resident` default and therefore keep their historical
    /// generating behavior.
    #[must_use]
    pub fn snapshot_terrain_if_resident<S: ChunkSource + ?Sized>(
        &self,
        source: &S,
    ) -> Option<Arc<ChunkWorld>> {
        let mut columns = Vec::with_capacity(self.chunks().len());
        for &(cx, cz) in self.chunks() {
            let column = source.resident_column(cx, cz).or_else(|| {
                source
                    .is_column_resident(cx, cz)
                    .then(|| source.column(cx, cz))
            })?;
            columns.push(((cx, cz), column));
        }
        Some(Arc::new(ChunkWorld::from_columns(columns)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::chunk::ChunkColumn;

    fn anchors_at(dimension: Dimension, cx: i32, cz: i32) -> TickAnchors {
        let anchors = TickAnchors::default();
        anchors.publish(vec![TickAnchor { dimension, cx, cz }]);
        anchors
    }

    fn follow(anchors: TickAnchors, dimension: Dimension, radius: i32) -> TickFollow {
        TickFollow {
            dimension,
            radius,
            anchors,
        }
    }

    struct SnapshotProbe {
        resident: bool,
        cold_calls: AtomicUsize,
    }

    impl ChunkSource for SnapshotProbe {
        fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            self.cold_calls.fetch_add(1, Ordering::Relaxed);
            ChunkColumn::new(0, 16)
        }

        fn resident_column(&self, _cx: i32, _cz: i32) -> Option<ChunkColumn> {
            self.resident.then(|| ChunkColumn::new(0, 16))
        }

        fn block_state_id(
            &self,
            _x: i32,
            _y: i32,
            _z: i32,
        ) -> lodestone_data::block_states::StateId {
            lodestone_data::block_states::StateId::AIR
        }

        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            crate::chunk::DEFAULT_BIOME.to_owned()
        }

        fn set_block(
            &self,
            _x: i32,
            _y: i32,
            _z: i32,
            _state: lodestone_data::block_states::StateId,
        ) {
        }

        fn is_column_resident(&self, _cx: i32, _cz: i32) -> bool {
            self.resident
        }
    }

    /// The control first proves that the probe would observe a cold load, then
    /// proves the resident-only seam returns no view without making one. This
    /// keeps the zero-call assertion meaningful rather than testing a source
    /// whose detector was never exercised.
    #[test]
    fn resident_snapshot_never_falls_back_to_cold_generation() {
        let area = FollowArea::new(
            TickFollow::default(),
            0..=0,
            0..=0,
        );
        let source = SnapshotProbe {
            resident: false,
            cold_calls: AtomicUsize::new(0),
        };

        let _ = area.snapshot_terrain(&source);
        assert_eq!(
            source.cold_calls.load(Ordering::Relaxed),
            1,
            "the control must demonstrate that the ordinary snapshot loads cold terrain",
        );
        source.cold_calls.store(0, Ordering::Relaxed);

        assert!(
            area.snapshot_terrain_if_resident(&source).is_none(),
            "a missing resident column must defer the snapshot",
        );
        assert_eq!(
            source.cold_calls.load(Ordering::Relaxed),
            0,
            "the resident-only snapshot must not call the generating API",
        );
    }

    #[test]
    fn resident_snapshot_copies_available_columns_without_loading() {
        let area = FollowArea::new(
            TickFollow::default(),
            0..=0,
            0..=0,
        );
        let source = SnapshotProbe {
            resident: true,
            cold_calls: AtomicUsize::new(0),
        };

        assert!(
            area.snapshot_terrain_if_resident(&source).is_some(),
            "a resident area must produce the terrain view",
        );
        assert_eq!(
            source.cold_calls.load(Ordering::Relaxed),
            0,
            "resident snapshots must copy rather than regenerate",
        );
    }

    /// **The discriminating input.** Every existing gate in this area spawns the
    /// player at chunk `(0, 0)`, which is the one position where "fixed at origin"
    /// and "follows the player" are the same set — so a gate there cannot fail
    /// under the old behaviour and proves nothing.
    ///
    /// This one puts the player at chunk `(100, -37)`: far from the origin, with a
    /// negative axis (so a truncating rather than flooring divide anywhere upstream
    /// shows), and `100 != -37` so the two axes cannot be transposed unnoticed.
    /// The assertion is written as *"the old area and the new one are disjoint"*,
    /// which is exactly the claim the fixed box fails.
    #[test]
    fn the_area_follows_a_player_far_from_the_origin() {
        let anchors = anchors_at(Dimension::Overworld, 100, -37);
        let mut area = FollowArea::new(follow(anchors, Dimension::Overworld, 3), -3..=3, -3..=3);

        // Before recompute it is the fallback — the old behaviour, kept for the
        // playerless callers.
        assert_eq!(area.chunks().len(), 49);
        assert!(
            area.chunks().contains(&(0, 0)),
            "the fallback must be the origin box every existing gate expects"
        );

        assert!(area.recompute(), "the set moved, so recompute must say so");
        assert_eq!(area.chunks().len(), 49, "still a 7x7 square, just elsewhere");
        assert!(area.chunks().contains(&(100, -37)), "centred on the player");
        assert!(area.chunks().contains(&(97, -40)), "and its far corner");
        assert!(area.chunks().contains(&(103, -34)), "and its other corner");
        // The claim the old behaviour fails: the origin is no longer ticked.
        assert!(
            !area.chunks().contains(&(0, 0)),
            "a fixed-at-origin area would still hold (0, 0); this is the assertion \
             the old behaviour cannot pass"
        );
        // And the transposed centre is not in it either, so (100, -37) was not
        // read as (-37, 100).
        assert!(
            !area.chunks().contains(&(-37, 100)),
            "the axes must not be transposed"
        );

        assert!(
            !area.recompute(),
            "a second recompute with an unmoved player must report no change — \
             this is what gates the terrain rebuild"
        );
    }

    #[test]
    fn an_empty_fallback_waits_for_the_first_player_anchor() {
        let anchors = TickAnchors::default();
        let mut area = FollowArea::new(follow(anchors.clone(), Dimension::Overworld, 1), 0..=-1, 0..=-1);

        assert!(area.chunks().is_empty(), "the primary join window must tick no cold columns");
        assert!(
            !area.recompute(),
            "an empty fallback remains empty until a player position arrives"
        );

        anchors.publish(vec![TickAnchor {
            dimension: Dimension::Overworld,
            cx: 100,
            cz: -37,
        }]);
        assert!(area.recompute(), "the first anchor must activate the tick area");
        assert_eq!(area.chunks().len(), 9);
        assert!(area.chunks().contains(&(100, -37)));
    }

    /// One chunk of movement adds a strip, not a new square, which is the property
    /// that makes the terrain rebuild affordable.
    ///
    /// Predicted exactly rather than asserted as "small". At radius 3 the square
    /// spans `cx ∈ [97, 103]`; after a `+x` step it spans `[98, 104]`, so the
    /// overlap is the **six** shared `cx` values × 7 `cz` = **42**, and exactly one
    /// new column (`cx = 104`) × 7 = **7** is gained.
    ///
    /// The first draft of this predicted `(35, 7)` — "it keeps 5×7" — which is the
    /// plausible round number rather than the derived one, and the gate failed on
    /// it. A `7`-wide span shares `7 - 1 = 6` columns with its one-step neighbour,
    /// not 5. Worth leaving on the record: this is the arithmetic that decides how
    /// many columns `snapshot_terrain` has to fetch, so guessing it low would
    /// under-estimate the very cost this design is built around.
    #[test]
    fn a_one_chunk_step_moves_only_a_seven_column_strip() {
        let anchors = anchors_at(Dimension::Overworld, 100, -37);
        let mut area = FollowArea::new(
            follow(anchors.clone(), Dimension::Overworld, 3),
            -3..=3,
            -3..=3,
        );
        area.recompute();
        let before: Vec<(i32, i32)> = area.chunks().to_vec();

        anchors.publish(vec![TickAnchor {
            dimension: Dimension::Overworld,
            cx: 101,
            cz: -37,
        }]);
        assert!(area.recompute());
        let after = area.chunks();

        let retained = before.iter().filter(|c| after.contains(c)).count();
        let gained = after.iter().filter(|c| !before.contains(c)).count();
        assert_eq!(
            (retained, gained),
            (42, 7),
            "a 7-wide square stepping one column shares 6 of its 7 cx values \
             (6 x 7 = 42) and gains exactly one new one (1 x 7 = 7)"
        );
        // And the complement, so the two halves cannot both be wrong in the same
        // direction: everything not retained was retired, and the total is 49.
        assert_eq!(retained + gained, 49, "the area is still 7x7 after the step");
    }

    /// A player in another dimension must not move this loop's area, and the
    /// fallback is what it lands on — vanilla's "no tickets, no ticking".
    ///
    /// The Nether anchor is at `(100, -37)`, the *same* coordinates the overworld
    /// test uses, so the only thing that can make this pass is the dimension check
    /// itself. Under a missing check the area would centre on overworld `(100, -37)`
    /// and this fails on the `(0, 0)` assertion.
    #[test]
    fn an_anchor_in_another_dimension_is_ignored() {
        let anchors = anchors_at(Dimension::Nether, 100, -37);
        let mut area = FollowArea::new(follow(anchors, Dimension::Overworld, 3), -3..=3, -3..=3);
        assert!(
            !area.recompute(),
            "the fallback was already in place, so nothing changed"
        );
        assert!(
            area.chunks().contains(&(0, 0)),
            "with no anchor in this dimension the area is the fallback"
        );
        assert!(
            !area.chunks().contains(&(100, -37)),
            "an anchor from another dimension must not drag this area to it"
        );
    }

    /// Two players in one dimension tick the union, and an overlapping column
    /// appears exactly once.
    ///
    /// The centres are three columns apart on `x`, so the squares overlap by
    /// `4 x 7 = 28` columns and the union is `49 + 49 - 28 = 70`. Predicted, not
    /// bounded: a duplicate column is a doubled random tick, i.e. a doubled crop
    /// growth rate, so "at most 98" would pass with the bug present.
    #[test]
    fn overlapping_players_tick_the_union_with_no_duplicates() {
        let anchors = TickAnchors::default();
        anchors.publish(vec![
            TickAnchor {
                dimension: Dimension::Overworld,
                cx: 100,
                cz: -37,
            },
            TickAnchor {
                dimension: Dimension::Overworld,
                cx: 103,
                cz: -37,
            },
        ]);
        let mut area = FollowArea::new(follow(anchors, Dimension::Overworld, 3), -3..=3, -3..=3);
        assert!(area.recompute());
        assert_eq!(area.chunks().len(), 70, "49 + 49 - 28 overlapping");
        let mut sorted = area.chunks().to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 70, "and no column appears twice");
        assert_eq!(
            area.spawn_cap_chunks(),
            17 * 20,
            "x spans 92..=108 and 95..=111, a 20-wide by 17-tall union, however few columns are simulated"
        );
    }

    /// A populated, two-player scene provides a deterministic spatial report
    /// without pretending that its candidate cells are already tick owners.
    /// The two radius-one squares lie on deliberately different sides of the
    /// 8-chunk boundaries, making an off-by-one or truncating negative divide
    /// change the exact eight-cell result below.
    #[test]
    fn a_populated_area_reports_candidate_region_workload_without_partitioning_the_tick() {
        let anchors = TickAnchors::default();
        anchors.publish(vec![
            TickAnchor {
                dimension: Dimension::Overworld,
                cx: -1,
                cz: -1,
            },
            TickAnchor {
                dimension: Dimension::Overworld,
                cx: 16,
                cz: 0,
            },
        ]);
        let mut area = FollowArea::new(follow(anchors, Dimension::Overworld, 1), -3..=3, -3..=3);
        assert!(area.recompute());

        let workload = area.candidate_region_workload(NonZeroU32::new(8).unwrap());

        assert_eq!(workload.total_chunks(), 18);
        assert_eq!(workload.largest_region_chunks(), 4);
        assert_eq!(
            workload.regions(),
            [
                crate::tick_region::CandidateRegionLoad {
                    region: (-1, -1),
                    chunks: 4,
                },
                crate::tick_region::CandidateRegionLoad {
                    region: (-1, 0),
                    chunks: 2,
                },
                crate::tick_region::CandidateRegionLoad {
                    region: (0, -1),
                    chunks: 2,
                },
                crate::tick_region::CandidateRegionLoad {
                    region: (0, 0),
                    chunks: 1,
                },
                crate::tick_region::CandidateRegionLoad {
                    region: (1, -1),
                    chunks: 1,
                },
                crate::tick_region::CandidateRegionLoad {
                    region: (1, 0),
                    chunks: 2,
                },
                crate::tick_region::CandidateRegionLoad {
                    region: (2, -1),
                    chunks: 2,
                },
                crate::tick_region::CandidateRegionLoad {
                    region: (2, 0),
                    chunks: 4,
                },
            ]
        );
        assert_eq!(area.spawn_cap_chunks(), 2 * 17 * 17, "x spans -9..=7 and 8..=24: adjacent, not overlapping");
    }

    #[test]
    fn chunk_owners_keep_negative_and_boundary_columns_in_visit_order() {
        let anchors = TickAnchors::default();
        anchors.publish(vec![
            TickAnchor {
                dimension: Dimension::Overworld,
                cx: -1,
                cz: 0,
            },
            TickAnchor {
                dimension: Dimension::Overworld,
                cx: 0,
                cz: 0,
            },
        ]);
        let mut area = FollowArea::new(follow(anchors, Dimension::Overworld, 0), -3..=3, -3..=3);
        assert!(area.recompute());

        assert_eq!(area.chunks(), [(-1, 0), (0, 0)]);
        assert_eq!(
            area.owned_chunks(),
            [
                TickOwnedChunk {
                    owner: crate::tick_region::TickOwner::Chunk { cx: -1, cz: 0 },
                    chunk: (-1, 0),
                },
                TickOwnedChunk {
                    owner: crate::tick_region::TickOwner::Chunk { cx: 0, cz: 0 },
                    chunk: (0, 0),
                },
            ]
        );
    }

    /// The visit order is stable across recomputes, because the random-tick
    /// stream's draw positions depend on it.
    #[test]
    fn the_column_order_is_stable() {
        let anchors = anchors_at(Dimension::Overworld, 100, -37);
        let mut a = FollowArea::new(
            follow(anchors.clone(), Dimension::Overworld, 3),
            -3..=3,
            -3..=3,
        );
        let mut b = FollowArea::new(follow(anchors, Dimension::Overworld, 3), -3..=3, -3..=3);
        a.recompute();
        b.recompute();
        assert_eq!(a.chunks(), b.chunks());
    }
}
