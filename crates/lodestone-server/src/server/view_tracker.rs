//! Per-connection chunk view tracking: which columns a client holds, in what order they stream in, and the ring-by-ring join batches.

use super::*;

/// Per-connection view-streaming bookkeeping: which chunk columns has this
/// connection been sent, and around which chunk column.
///
/// Mirrors vanilla's own chunk-map/chunk-tracking-view machinery
/// (its own "update chunk tracking"/"apply chunk tracking view" steps,
/// its own tracking-view "difference" helper), simplified to the same square
/// window `serve_connection`'s own initial view already uses
/// (`[-view_radius, view_radius]²`) rather than vanilla's rounded
/// positioned-tracking-view "contains" check (a buffered Euclidean-distance
/// test). Keeping the join-time and move-time shapes identical is what stops
/// a live connection from immediately forgetting chunks it only just
/// finished sending at join; matching vanilla's exact circular shape is not
/// otherwise load-bearing for "the world keeps up as the player walks".
#[derive(Debug)]
pub(super) struct ViewTracker {
    pub(super) center: (i32, i32),
    pub(super) loaded: HashSet<(i32, i32)>,
    pub(super) delivered: HashSet<(i32, i32)>,
    pub(super) columns: HashMap<(i32, i32), ViewColumn>,
    pub(super) next_incarnation: u64,
    /// The optional moving complete-generation band. `None` preserves the
    /// historic all-full stream used by borrowed and cross-dimension joins;
    /// `Some` is the progressive shared-source path.
    pub(super) generation_band: Option<((i32, i32), i32)>,
    /// The connection's *current* effective view radius — starts at the radius
    /// the connection joined with (`serve_connection`'s own `view_radius`
    /// parameter) and can shrink or grow within
    /// [`max_radius`](Self::max_radius) via
    /// [`set_view_radius`](Self::set_view_radius) (the view-distance
    /// `ServerBound::ClientInformationChanged`). Stored on `self` rather than
    /// re-passed at every [`recenter`](Self::recenter) call so a client's
    /// requested distance actually sticks across subsequent moves, instead
    /// of being silently overwritten by the original radius on the next
    /// `PlayerMoved`.
    pub(super) radius: i32,
    /// The largest radius this connection is **permitted** to reach, and the
    /// ceiling [`set_view_radius`](Self::set_view_radius) clamps a client
    /// request to the configured ceiling, keeping the advertised distance
    /// within the server's accepted range.
    ///
    /// **The view-distance ceiling is a second field because it answers a second
    /// question.** `radius` above is where the connection *starts*; this is how
    /// far it may *go*. A client may lower or raise its requested distance within
    /// the configured ceiling, which is a server setting rather than the player's
    /// current view.
    ///
    /// Who supplies it is a per-path memory-policy decision, the same fork
    /// `ChunkStore::for_view_radius` vs `for_integrated_view_radius` already
    /// encodes: singleplayer (`open_in_memory*`) passes
    /// [`MAX_CLIENT_VIEW_RADIUS`] because it is the slider-mover's own memory,
    /// while open-to-LAN (`IntegratedServer::bind`) passes its configured
    /// `view_radius` because it spends an operator's memory on behalf of players
    /// who did not choose the setting. Every other caller passes `view_radius`,
    /// which preserves the compatibility behavior of those callers.
    pub(super) max_radius: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ColumnIncarnation(pub(super) u64);

#[derive(Debug)]
pub(super) struct ViewColumn {
    pub(super) incarnation: ColumnIncarnation,
    pub(super) requested: ChunkGenerationStage,
    pub(super) served: Option<ChunkGenerationStage>,
}

#[derive(Debug)]
pub(super) struct EncodedColumn {
    pub(super) directive: ServerDirective,
    pub(super) stage: Option<ChunkGenerationStage>,
}

pub(super) type PendingJoinEncodes<'a> = crate::join_scheduler::OrderedJoinEncodes<
    'a, (ColumnIncarnation, EncodedColumn),
>;

/// Resolves the two-stage streaming policy for one coordinate. The arithmetic
/// is intentionally local to the server-side ledger so the tracker and the
/// scheduler can be tested independently while sharing the same Chebyshev band
/// definition.
pub(super) fn stage_for_band(
    band: Option<((i32, i32), i32)>,
    coord: (i32, i32),
) -> ChunkGenerationStage {
    match band {
        Some(((cx, cz), radius))
            if (coord.0 - cx).abs().max((coord.1 - cz).abs()) > radius =>
        {
            ChunkGenerationStage::Shaped
        }
        _ => ChunkGenerationStage::Full,
    }
}

/// The directives produced by one [`ViewTracker`] update, split by whether
/// they are subject to the chunk-batch flow-control gate
/// (`ServerBound::ChunkBatchAcknowledged`) — see
/// [`send_view_update`]'s own doc comment for how a caller applies this.
#[derive(Debug, Default)]
pub(super) struct ViewUpdate {
    /// Cache-center updates and forgets are sent right away regardless of any
    /// outstanding chunk-batch acknowledgement. Only new chunk sends wait for
    /// that flow-control signal.
    pub(super) immediate: Vec<ServerDirective>,
    /// The columns that left the view — the same set `immediate`'s
    /// `encode_forget_chunk` directives name, kept as coordinates as well so
    /// [`send_view_update`] can withdraw any of them still queued in the column
    /// stream. See [`crate::join_scheduler::ColumnPipeline::cancel`] for why an owed
    /// column can outlive its own forget.
    pub(super) forgotten: HashSet<(i32, i32)>,
    /// The newly-visible columns, in wire order — empty when nothing new entered
    /// the view.
    ///
    /// **Coordinates, not directives.** This API returns work for the caller to
    /// schedule rather than a finished
    /// `begin_chunk_batch`/`encode_chunk`*/`end_chunk_batch` sequence, which meant
    /// [`ViewTracker::recenter`] had to `await` the generation *and* the encode of
    /// every column in the strip before returning: one suspension point covering
    /// `2r + 1` columns, 33 at `view_radius = 16`, during which the connection
    /// task read nothing and wrote nothing. Handing back the coordinates makes
    /// both update paths synchronous and lets [`send_view_update`] feed them to
    /// the same streaming pipeline the join uses, one column per pass of the
    /// `select!` loop.
    pub(super) added: Vec<(i32, i32)>,
    /// Loaded columns still owed a complete packet which have entered the
    /// complete-generation band. These are whole-column resends, not block
    /// updates, so the client's normal `World::load`/remesh path replaces the
    /// partial terrain and schedules all of its meshes.
    pub(super) upgrades: Vec<(i32, i32)>,
}

impl ViewTracker {
    /// Seeds the tracker with the square already sent for the initial join
    /// view (`center`, `[-view_radius, view_radius]²` around it), so the
    /// first [`recenter`](Self::recenter) diffs against what the client
    /// actually has rather than an empty set.
    /// `max_view_radius` is the ceiling for a *later*
    /// [`set_view_radius`](Self::set_view_radius) — see
    /// [`max_radius`](Self::max_radius). It is raised to `view_radius` if a
    /// caller passes something smaller, because the join square has already been
    /// sent and a ceiling under it would make the connection's very first live
    /// settings packet shrink a view nobody asked to shrink.
    pub(super) fn new(center: (i32, i32), view_radius: i32, max_view_radius: i32) -> Self {
        Self::new_with_generation_band(center, view_radius, max_view_radius, None)
    }

    /// Seeds a tracker for a progressive join. Columns inside `full_radius`
    /// are requested as `Full`; the rest are requested as `Shaped`, matching the
    /// stages the join pipeline requests. A later recenter can therefore
    /// distinguish a genuinely new column from an already-loaded partial one.
    pub(super) fn new_banded(
        center: (i32, i32),
        view_radius: i32,
        max_view_radius: i32,
        full_radius: i32,
    ) -> Self {
        Self::new_with_generation_band(
            center,
            view_radius,
            max_view_radius,
            Some((center, full_radius)),
        )
    }

    pub(super) fn new_with_generation_band(
        center: (i32, i32),
        view_radius: i32,
        max_view_radius: i32,
        generation_band: Option<((i32, i32), i32)>,
    ) -> Self {
        let mut loaded = HashSet::new();
        for dz in -view_radius..=view_radius {
            for dx in -view_radius..=view_radius {
                loaded.insert((center.0 + dx, center.1 + dz));
            }
        }
        let columns = loaded
            .iter()
            .copied()
            .enumerate()
            .map(|(index, coord)| (coord, ViewColumn {
                incarnation: ColumnIncarnation(index as u64),
                requested: stage_for_band(generation_band, coord),
                served: None,
            }))
            .collect();
        let next_incarnation = loaded.len() as u64;
        Self {
            center,
            loaded,
            delivered: HashSet::new(),
            columns,
            next_incarnation,
            generation_band,
            radius: view_radius,
            max_radius: max_view_radius.max(view_radius),
        }
    }

    /// The stage a streamed coordinate is owed under the current band. A
    /// tracker without a band is the all-full compatibility path.
    pub(super) fn stage_for(&self, coord: (i32, i32)) -> ChunkGenerationStage {
        stage_for_band(self.generation_band, coord)
    }

    pub(super) fn mark_delivered(&mut self, coord: (i32, i32)) {
        if self.loaded.contains(&coord) {
            self.delivered.insert(coord);
        }
    }

    pub(super) fn incarnation(&self, coord: (i32, i32)) -> Option<ColumnIncarnation> {
        self.columns.get(&coord).map(|column| column.incarnation)
    }

    pub(super) fn is_current(&self, coord: (i32, i32), incarnation: ColumnIncarnation) -> bool {
        self.incarnation(coord) == Some(incarnation)
    }

    pub(super) fn needs_delivery(
        &self,
        coord: (i32, i32),
        incarnation: ColumnIncarnation,
        stage: Option<ChunkGenerationStage>,
    ) -> bool {
        self.columns.get(&coord).is_some_and(|column| {
            column.incarnation == incarnation
                && stage.is_none_or(|stage| column.served.is_none_or(|served| served < stage))
        })
    }

    pub(super) fn record_delivery(
        &mut self,
        coord: (i32, i32),
        incarnation: ColumnIncarnation,
        stage: Option<ChunkGenerationStage>,
    ) {
        if !self.is_current(coord, incarnation) {
            return;
        }
        if let Some(stage) = stage {
            let column = self.columns.get_mut(&coord).expect("current view column exists");
            column.served = Some(column.served.map_or(stage, |served| served.max(stage)));
        }
        self.mark_delivered(coord);
    }

    pub(super) fn reserve_column(&mut self, coord: (i32, i32), stage: ChunkGenerationStage) {
        let incarnation = ColumnIncarnation(self.next_incarnation);
        self.next_incarnation = self.next_incarnation.checked_add(1)
            .expect("column incarnation space exhausted");
        self.columns.insert(coord, ViewColumn { incarnation, requested: stage, served: None });
    }

    pub(super) fn reset(&mut self, center: (i32, i32)) {
        self.center = center;
        self.generation_band = None;
        self.loaded = Self::window(center, self.radius);
        self.delivered.clear();
        self.columns.clear();
        let coordinates = self.loaded.iter().copied().collect::<Vec<_>>();
        for coord in coordinates {
            self.reserve_column(coord, ChunkGenerationStage::Full);
        }
    }

    /// The square `[-self.radius, self.radius]²` window around `center`.
    pub(super) fn window(center: (i32, i32), radius: i32) -> HashSet<(i32, i32)> {
        let mut next = HashSet::new();
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                next.insert((center.0 + dx, center.1 + dz));
            }
        }
        next
    }

    /// Every column in `next` this tracker has not sent, in wire order — empty if
    /// there is nothing visible to add. Shared by [`recenter`](Self::recenter) and
    /// [`set_view_radius`](Self::set_view_radius) so both diff against
    /// `self.loaded` identically.
    pub(super) fn added_columns(
        &self,
        next: &HashSet<(i32, i32)>,
        centre: (i32, i32),
        facing: Option<f32>,
    ) -> Vec<(i32, i32)> {
        // Sorted rather than left in `HashSet::difference`'s hash-iteration
        // order: that order already varies run-to-run (`RandomState` reseeds
        // per process), and generating in parallel below means the set of
        // columns can finish in yet another, scheduling-dependent order.
        // Fixing the wire order here is what makes the encoded byte sequence
        // independent of both.
        //
        // **Ordered nearest-first, not lexicographically.** A bare
        // `sort_unstable()` orders by `cx` then `cz` — a raster walk, so a player
        // walking east would get the visible column strip filled from its
        // northern end regardless of where along it they actually were. The same
        // key the join stream uses (`join_scheduler::view_order_key`: distance
        // from the player's column first, the cone they are looking down second)
        // makes a *move* behave like a join. It is a total order over
        // integers, so the wire order stays a
        // deterministic function of the pose, not of scheduling.
        let mut added: Vec<(i32, i32)> = next.difference(&self.loaded).copied().collect();
        added.sort_unstable_by_key(|&coord| {
            crate::join_scheduler::view_order_key(centre, facing, coord)
        });
        added
    }

    /// Loaded columns that remain in `next` but have just crossed into the
    /// complete-generation band. The result is sorted with the same nearest-
    /// first key as new additions, making both request classes deterministic.
    pub(super) fn upgrade_columns(
        &self,
        next: &HashSet<(i32, i32)>,
        centre: (i32, i32),
        facing: Option<f32>,
    ) -> Vec<(i32, i32)> {
        let Some((_, _)) = self.generation_band else {
            return Vec::new();
        };
        let mut upgrades: Vec<(i32, i32)> = self
            .loaded
            .intersection(next)
            .copied()
            .filter(|coord| {
                self.columns.get(coord).is_some_and(|column| {
                    column.requested < ChunkGenerationStage::Full
                        && column.served != Some(ChunkGenerationStage::Full)
                })
                    && stage_for_band(
                        self.generation_band.map(|(_, radius)| (centre, radius)),
                        *coord,
                    ) == ChunkGenerationStage::Full
            })
            .collect();
        upgrades.sort_unstable_by_key(|&coord| {
            crate::join_scheduler::view_order_key(centre, facing, coord)
        });
        upgrades
    }

    /// Recomputes the view for a new player chunk position `(cx, cz)` at the
    /// tracker's current [`radius`](Self::radius), returning the directives
    /// that bring the client's tracked chunks back in sync — and returning
    /// nothing at all if `(cx, cz)` is still the tracked center (the same
    /// "did the 2D chunk position actually change" guard
    /// vanilla's own "update chunk tracking" step applies before touching the view at
    /// all).
    ///
    /// Order mirrors vanilla's own "apply chunk tracking view" step: the cache-center update is sent first
    /// (unconditionally, since by this point the center *did* change —
    /// vanilla additionally guards this send on the center changing, which
    /// is already implied here), then every column that left the window is
    /// forgotten, then every column that entered it is sent as one chunk
    /// batch.
    ///
    /// `facing` is the player's yaw in degrees where the connection has reported
    /// one, and only orders the added columns (see
    /// [`added_columns`](Self::added_columns)) — never *which* columns they are,
    /// which is the square alone.
    ///
    /// **Synchronous.** It computes a set difference and encodes forgets; nothing
    /// here generates terrain, which is what lets a chunk-boundary crossing return
    /// to the `select!` loop immediately instead of parking it for the length of a
    /// 33-column strip. [`send_view_update`] is where the columns become bytes.
    pub(super) fn recenter<P>(&mut self, proto: &P, cx: i32, cz: i32, facing: Option<f32>) -> ViewUpdate
    where
        P: ServerProtocol,
    {
        if (cx, cz) == self.center {
            return ViewUpdate::default();
        }

        let next = Self::window((cx, cz), self.radius);

        let mut immediate = vec![proto.encode_chunk_cache_center(cx, cz)];
        let forgotten: HashSet<(i32, i32)> = self.loaded.difference(&next).copied().collect();
        for &(x, z) in &forgotten {
            immediate.push(proto.encode_forget_chunk(x, z));
        }
        let added = self.added_columns(&next, (cx, cz), facing);
        let upgrades = self.upgrade_columns(&next, (cx, cz), facing);
        let next_band = self
            .generation_band
            .map(|(_, radius)| ((cx, cz), radius));

        self.center = (cx, cz);
        self.loaded = next;
        self.delivered.retain(|coord| self.loaded.contains(coord));
        self.generation_band = next_band;
        for coord in &forgotten {
            self.columns.remove(coord);
        }
        for &coord in &added {
            self.reserve_column(coord, stage_for_band(next_band, coord));
        }
        for &coord in &upgrades {
            self.columns.get_mut(&coord).expect("upgrade remains loaded").requested = ChunkGenerationStage::Full;
        }
        ViewUpdate {
            immediate,
            forgotten,
            added,
            upgrades,
        }
    }

    /// Resizes the tracked view around the *current* center without the
    /// player having moved at all (the `ClientInformationChanged` view-distance
    /// `ServerBound::ClientInformationChanged` — a client changing its
    /// render-distance setting mid-session). Unlike
    /// [`recenter`](Self::recenter), there is no cache-center update to send
    /// (the center did not change) and no early-return guard on position —
    /// the guard here is `radius` itself already matching, so a settings
    /// packet that does not actually change the distance is correctly a
    /// no-op.
    pub(super) fn set_view_radius<P, S>(
        &mut self,
        proto: &P,
        source: SourceRef<'_, S>,
        radius: i32,
        facing: Option<f32>,
    ) -> ViewUpdate
    where
        P: ServerProtocol,
        S: ChunkSource + 'static,
    {
        // Clamp against the configured ceiling rather than the connection's
        // current radius. The lower bound is `0`, which represents an empty
        // view and keeps the server-side range valid for small test worlds.
        // `.max(0)` on the ceiling preserves `clamp`'s `min <= max` invariant
        // when a caller supplies a negative configuration value.
        let radius = radius.clamp(0, self.max_radius.max(0));
        if radius == self.radius {
            return ViewUpdate::default();
        }

        // Resize the retention bound *before* streaming the requested view.
        // A larger radius can evict the **innermost** ring while
        // `join_view_rings` streams outward; regenerating the ground under the
        // player's feet costs ~909 ms per column. Doing this first keeps every
        // column in the `added` set resident until generation completes. For a
        // source that retains nothing per view, the call is a no-op; see
        // `ChunkSource::set_retention_radius`.
        source.get().set_retention_radius(radius);

        let next = Self::window(self.center, radius);
        let mut immediate = Vec::new();
        let forgotten: HashSet<(i32, i32)> = self.loaded.difference(&next).copied().collect();
        for &(x, z) in &forgotten {
            immediate.push(proto.encode_forget_chunk(x, z));
        }
        // Centred on the tracker's own centre, which by definition did not move
        // here — a render-distance change is the one view update with no new pose.
        let added = self.added_columns(&next, self.center, facing);
        let upgrades = self.upgrade_columns(&next, self.center, facing);

        self.radius = radius;
        self.loaded = next;
        self.delivered.retain(|coord| self.loaded.contains(coord));
        for coord in &forgotten {
            self.columns.remove(coord);
        }
        for &coord in &added {
            self.reserve_column(coord, self.stage_for(coord));
        }
        for &coord in &upgrades {
            self.columns.get_mut(&coord).expect("upgrade remains loaded").requested = ChunkGenerationStage::Full;
        }
        ViewUpdate {
            immediate,
            forgotten,
            added,
            upgrades,
        }
    }
}

/// The join view, split into **Chebyshev rings** ordered outward from the
/// player's own column — the join stream.
///
/// Ring `r` is every column at Chebyshev (chess-king) distance exactly `r` from
/// the centre, so ring 0 is the single column the player is standing in, ring 1
/// is the 8 around it, and ring `r > 0` holds `8r` columns. Flattened, the
/// result is the whole `[-view_radius, view_radius]²` square with **no column
/// repeated and none missing**.
///
/// # These are offsets, not chunk coordinates
///
/// Every pair is a `(dx, dz)` **relative to the player's own column**, which is
/// why ring 0 is `(0, 0)` at every view radius. The caller must add the join
/// centre before any of it reaches `encode_chunk` or a chunk source. This
/// produces the same set that `ViewTracker::new` seeds, in a different order;
/// the property belongs to the call site, not this function.
///
/// # Why rings
///
/// The join enumerates Chebyshev rings rather than raster order from
/// `(-view_radius, -view_radius)`. With 361 columns, the player's own column
/// is first, so terrain near the player is encoded promptly. A raster walk
/// places that column around item **~180 of 361**.
///
/// `crate::join_scheduler` flattens these groups and drives a primed sliding
/// window over the result, so the first chunk reaches the client after **one**
/// column of generation while nothing waits on a ring boundary. This function
/// is therefore purely the **wire order**. The grouping states that order
/// clearly, and `join_view_rings_partitions_the_square_exactly` verifies the
/// partition without synchronizing the groups.
///
/// `ViewTracker::build_batch` — the *move*-time counterpart — orders on the
/// same distance-first key rather than a lexicographic `sort_unstable`, so
/// walking into terrain fills nearest-first like joining does. The key is a
/// total order over integers derived from the player's pose, so determinism is
/// preserved.
///
/// The protocol's chunk priority also spirals outward, with ticket level as the
/// priority. This is the measured distance-first slice of that behavior.
///
/// # Determinism
///
/// Order **within** a ring is the same `dz`-outer/`dx`-inner walk the whole
/// square uses, filtered to the ring. So the emitted byte sequence stays
/// a pure function of `view_radius` — independent of thread scheduling, hash
/// seeds, and which arm of [`SourceRef`] generated it.
///
/// # Cost
///
/// One column. Ring 0 is generated alone, which buys the one-column
/// time-to-first-chunk; from the second column onward the in-flight window
/// spans ring boundaries freely. This keeps worker utilization independent
/// of the slowest column in each ring.
pub(super) fn join_view_rings(view_radius: i32) -> Vec<Vec<(i32, i32)>> {
    // A negative radius yields **no rings**, not ring 0. `view_radius.max(0)`
    // reads as the harmless guard and is not: the raster walk this replaced built
    // `(-r..=r)`, which is an *empty* range for `r < 0`, so a negative radius
    // sent zero chunks. Clamping to 0 would send one — and `ViewTracker::new`
    // would still record an empty loaded set for the same input, so the tracker
    // and the wire would disagree about a column the client actually has.
    // Nothing produces a negative radius today (`dispatch_play_packet` clamps
    // with `view_radius.max(0)` precisely as an invariant against it), which is
    // exactly why the divergence would have gone unnoticed.
    if view_radius < 0 {
        return Vec::new();
    }
    (0..=view_radius)
        .map(|r| {
            let mut ring = Vec::new();
            for dz in -r..=r {
                for dx in -r..=r {
                    if dx.abs().max(dz.abs()) == r {
                        ring.push((dx, dz));
                    }
                }
            }
            ring
        })
        .collect()
}

/// Only the target column is emitted before the play loop starts. Its packet
/// encoder still admits and settles the radius-one light footprint first;
/// neighbouring columns remain in the deferred stream.
pub(super) const JOIN_PRESTREAM_RADIUS: i32 = 0;

/// How many columns of the deferred join stream `serve_play` puts in one chunk
/// batch.
///
/// The deferred half is batched because a stream spans ticks and the batch
/// markers cannot remain open across other play-loop traffic. This matches the
/// standard pacing shape, and the client answers each
/// `chunk_batch_finished` with a `chunk_batch_received` carrying its desired rate
/// (`crates/protocol/v770/src/adapter.rs`'s `ChunkBatchState`).
///
/// 16 is a compromise with no measurement behind it and does not need one: a
/// batch marker is two empty-ish packets, so the cost is ~2 packets per 16
/// columns, and the *only* thing the size changes is the granularity of the
/// client's own rate estimate.
pub(super) const JOIN_STREAM_BATCH_COLUMNS: usize = 16;

pub(super) struct PendingChunkBatch {
    pub(super) columns: Vec<((i32, i32), ColumnIncarnation, EncodedColumn)>,
}

pub(super) async fn send_encoded_column<T: Transport>(
    conn: &mut Connection<T>,
    state: &mut State,
    view: &mut ViewTracker,
    coord: (i32, i32),
    incarnation: ColumnIncarnation,
    encoded: EncodedColumn,
) -> Result<bool, ServerError> {
    if !view.needs_delivery(coord, incarnation, encoded.stage) {
        return Ok(false);
    }
    let sent = matches!(encoded.directive, ServerDirective::Send { .. });
    apply(conn, state, encoded.directive).await?;
    if sent {
        view.record_delivery(coord, incarnation, encoded.stage);
    }
    Ok(sent)
}

pub(super) async fn send_pending_chunk_batch<T: Transport, P: ServerProtocol>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    view: &mut ViewTracker,
    batch: PendingChunkBatch,
) -> Result<(), ServerError> {
    let columns = batch.columns.into_iter()
        .filter(|(coord, incarnation, encoded)| view.needs_delivery(*coord, *incarnation, encoded.stage))
        .collect::<Vec<_>>();
    if columns.is_empty() {
        return Ok(());
    }
    apply(conn, state, proto.begin_chunk_batch()).await?;
    let mut count = 0;
    for (coord, incarnation, encoded) in columns {
        count += i32::from(send_encoded_column(conn, state, view, coord, incarnation, encoded).await?);
    }
    apply(conn, state, proto.end_chunk_batch(count)).await
}

/// Applies one [`ViewUpdate`]: the cache-center and forget directives right away,
/// then the newly-visible columns.
///
/// # The two ways the columns get sent, and why the first is the point
///
/// **Preferred: hand them to `stream`.** `serve_play` drains a
/// [`JoinChunkStream`](crate::join_scheduler::JoinChunkStream) from a `select!`
/// branch one column at a time. The strip is generated on the blocking pool with
/// a primed window, re-keyed as the player moves, and interleaved with connection
/// reads and writes.
///
/// **Fallback: build the batch here.** `stream` refuses on its `Ringed` arm (a
/// borrowed, non-`'static` source — protocol tests) and when the caller has no
/// stream at all. Those
/// generate, encode, and send under
/// `awaiting_chunk_batch_ack`, the one-batch-in-flight gate
/// `ServerBound::ChunkBatchAcknowledged` closes.
///
/// The streamed path is deliberately **not** subject to that gate, because the join
/// stream it shares is not: batches there are paced by `JOIN_STREAM_BATCH_COLUMNS`
/// and the client's own `chunk_batch_received` rate estimate. Routing a move through
/// the gate *and* the stream would need two flow-control regimes over one ordered
/// queue, which is how the two paths drift.
pub(super) async fn send_view_update<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: SourceRef<'_, S>,
    stream: Option<&mut crate::join_scheduler::JoinChunkStream<S>>,
    state: &mut State,
    view: &mut ViewTracker,
    update: ViewUpdate,
    awaiting_chunk_batch_ack: &mut bool,
    pending_chunk_batches: &mut VecDeque<PendingChunkBatch>,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
{
    for directive in update.immediate {
        apply(conn, state, directive).await?;
    }
    // Asked, not attempted: a stream that refused *after* taking the coordinates
    // would leave the fallback below with nothing to send. See
    // `JoinChunkStream::accepts_enqueue`.
    if let Some(stream) = stream {
        if stream.accepts_enqueue() {
            // Withdraw before enqueueing, and unconditionally — a shrink forgets columns
            // and adds none, and those still owed must be dropped just the same. See
            // `ColumnPipeline::cancel`.
            stream.cancel(&update.forgotten);
            stream.enqueue(update.added);
            stream.enqueue_full(update.upgrades);
            return Ok(());
        }
        // Borrowed/ringed streams cannot accept new work. Remove an upgrade from
        // their still-buffered old stream before the direct full resend below;
        // otherwise the stale shaped payload could arrive after the upgrade and
        // downgrade the client again. The windowed arm returned above keeps both
        // classes in one queue and never reaches this branch.
        let upgrades: HashSet<(i32, i32)> = update.upgrades.iter().copied().collect();
        stream.cancel(&upgrades);
    }
    if update.added.is_empty() && update.upgrades.is_empty() {
        return Ok(());
    }
    let mut batch = Vec::new();
    let mut requested = update.added;
    requested.extend(update.upgrades);
    // Only a one-column protocol can offload (a borrowed source is not `'static'`):
    // cross-column and retained-initial protocols must settle their source state inline.
    let offloaded = if proto.uses_cross_column_light() || proto.retains_initial_column_light() {
        None
    } else {
        match source {
            SourceRef::Shared(src) => {
                crate::chunk::generate_and_encode_columns_offloaded(
                    Arc::clone(src),
                    requested.clone(),
                    proto.chunk_encoder(),
                )
                .await
            }
            SourceRef::Dimension(src) => {
                crate::chunk::generate_and_encode_columns_offloaded(
                    Arc::clone(src),
                    requested.clone(),
                    proto.chunk_encoder(),
                )
                .await
            }
            SourceRef::Borrowed(_) => None,
        }
    };
    match offloaded {
        Some(Ok(frames)) => {
            for (coord, directive) in requested.iter().copied().zip(frames) {
                if let Some(incarnation) = view.incarnation(coord) {
                    batch.push((coord, incarnation, EncodedColumn { directive, stage: None }));
                }
            }
        }
        Some(Err(error)) => {
            return return_chunk_encode_error(conn, proto, state, None, error).await;
        }
        None => {
            let columns = source.generate(requested.clone()).await?;
            for (&(x, z), column) in requested.iter().zip(columns.iter()) {
                if proto.uses_cross_column_light() || proto.retains_initial_column_light() {
                    source
                        .admit_columns(column_admission_footprint(
                            x,
                            z,
                            i32::from(proto.uses_cross_column_light()),
                        ))
                        .await?;
                }
                match encode_chunk_with_source_receipt(proto, source.get(), x, z, column) {
                    Ok(encoded) => {
                        if let Some(incarnation) = view.incarnation((x, z)) {
                            batch.push(((x, z), incarnation, encoded));
                        }
                    }
                    Err(error) => {
                        return return_chunk_encode_error(conn, proto, state, None, error).await;
                    }
                }
            }
        }
    }
    if *awaiting_chunk_batch_ack {
        pending_chunk_batches.push_back(PendingChunkBatch {
            columns: batch,
        });
        return Ok(());
    }
    *awaiting_chunk_batch_ack = true;
    send_pending_chunk_batch(conn, proto, state, view, PendingChunkBatch { columns: batch }).await
}

#[cfg(test)]
mod tests;
