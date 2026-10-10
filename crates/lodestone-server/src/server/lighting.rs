//! Light delivery to clients: resending columns after light changes, batched relight computation, and lighting for a single edit.

use super::*;

/// Re-sends the column owning `pos` when an edit changed the light that cell
/// emits, so the client's block light follows a placed or broken torch.
///
/// A no-op unless [`crate::light::should_relight`] fires — read that module's doc
/// comment first: it records what the served-light path was measured to actually
/// compute, why this function uses a whole-column resend rather than the `LIGHT_UPDATE`
/// packet that would be cheaper, and the two gaps this leaves (sky light after an
/// edit, and light crossing a column border).
///
/// `source.column(cx, cz)` reflects the `set_block` the caller already performed
/// That contract means the light is computed over terrain
/// that contains the torch.
///
/// # It is a `light_update`, not a column resend
///
/// The stopgap this replaces re-encoded the **whole column**: ~40 KiB on the wire
/// and 62 M instructions of `encode_chunk`, per placed torch, on the connection
/// task. `ServerProtocol::encode_light_update` is the real packet — a few KiB of
/// nibble arrays — and it needs no chunk batch, because vanilla's
/// The player chunk sender flow control counts chunk *batches* and `light_update` is
/// not one. Vanilla sends it the same way, ungated, from
/// The chunk map's light listener.
///
pub(super) async fn resend_column_for_light<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    pending_relights: Option<&mut PendingRelights>,
    old_state: StateId,
    new_state: StateId,
    pos: BlockPos,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    if !crate::light::should_relight(old_state, new_state) {
        return Ok(());
    }
    if let Some(pending_relights) = pending_relights {
        pending_relights.enqueue_edit(
            pos.x.div_euclid(16),
            pos.z.div_euclid(16),
            i32::from(proto.uses_cross_column_light()),
        );
        return Ok(());
    }
    send_lighting_for_edit(
        conn,
        proto,
        source,
        state,
        pos.x.div_euclid(16),
        pos.z.div_euclid(16),
    )
    .await
}

/// Recompute and send one column's light after a caller has established that an
/// edit changed emission or dampening, or when the prior block state is unknown.
pub(super) async fn send_column_light<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    cx: i32,
    cz: i32,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    let dimension = source
        .dimension()
        .unwrap_or(crate::dimension::Dimension::Overworld);
    let (column, light) = if proto.retains_initial_column_light() {
        let neighbour_offsets = light_neighbour_offsets(proto.uses_cross_column_light());
        let mut fallback = source.column(cx, cz);
        'settle: {
            for attempt in 0..=LIGHT_SETTLEMENT_OPTIMISTIC_RETRIES {
            let exclusive = attempt == LIGHT_SETTLEMENT_OPTIMISTIC_RETRIES;
            let mut compute = |candidate: &ChunkColumn,
                               neighbours: &[(i32, i32, &ChunkColumn)]| {
                if proto.uses_cross_column_light() {
                    proto.compute_column_light_with_neighbours_in_dimension(
                        candidate,
                        neighbours,
                        dimension,
                    )
                } else {
                    proto.compute_column_light_in_dimension(candidate, dimension)
                }
            };
            match source.settle_resident_column_light_with_neighbours(
                cx,
                cz,
                &fallback,
                &neighbour_offsets,
                false,
                true,
                exclusive,
                &mut compute,
            ) {
                Ok(column) => break 'settle (column.clone(), column.centre_settled_light().cloned()),
                Err(ColumnLightSettlementError::NoLight)
                | Err(ColumnLightSettlementError::MissingFootprint) => {
                    break 'settle (fallback, None)
                }
                Err(ColumnLightSettlementError::Conflict) if !exclusive => {
                    fallback = source.column(cx, cz);
                }
                Err(ColumnLightSettlementError::Conflict) => return Ok(()),
            }
            }
            unreachable!("the bounded dynamic-light settlement loop always returns")
        }
    } else {
        let column = source.column(cx, cz);
        // Both halves have to be present for the cheap path: a family that can
        // compute light but not encode the packet (or the reverse) would
        // otherwise silently send nothing, which is the exact island this
        // replaces.
        let light = if proto.uses_cross_column_light() {
            let neighbours = (-1..=1)
                .flat_map(|dz| (-1..=1).map(move |dx| (dx, dz)))
                .filter(|&(dx, dz)| (dx, dz) != (0, 0))
                .map(|(dx, dz)| (dx, dz, source.column(cx + dx, cz + dz)))
                .collect::<Vec<_>>();
            let neighbour_refs = borrowed_neighbours(&neighbours);
            proto.compute_column_light_with_neighbours_in_dimension(
                &column,
                &neighbour_refs,
                dimension,
            )
        } else {
            proto.compute_column_light_in_dimension(&column, dimension)
        };
        (column, light)
    };
    if let Some(light) = light {
        let directive = proto.encode_light_update(cx, cz, &light);
        if !matches!(directive, ServerDirective::None) {
            apply(conn, state, directive).await?;
            return Ok(());
        }
    }
    // Fallback: the whole-column resend, inside the same
    // `begin_chunk_batch`/`end_chunk_batch` pair every other chunk send in this
    // module uses — the batch accounting counts these directives, so a bare
    // `encode_chunk` outside one leaves the client's accounting short.
    apply(conn, state, proto.begin_chunk_batch()).await?;
    let packet_column = column_for_initial_encode(&column);
    let directive = match proto.try_encode_chunk_in_dimension(cx, cz, &packet_column, dimension) {
        Ok(directive) => directive,
        Err(error) => return return_chunk_encode_error(conn, proto, state, Some(0), error).await,
    };
    apply(conn, state, directive).await?;
    apply(conn, state, proto.end_chunk_batch(1)).await?;
    Ok(())
}

/// Clones the exact footprint needed for one live light update without turning
/// an unloaded neighbour into a synchronous generation request.
pub(super) fn resident_light_neighbourhood<S>(
    source: &S,
    cx: i32,
    cz: i32,
    radius: i32,
) -> Option<(ChunkColumn, Vec<(i32, i32, ChunkColumn)>)>
where
    S: ChunkSource + ?Sized,
{
    let column = resident_column(source, cx, cz)?;
    let mut neighbours = Vec::new();
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            if (dx, dz) != (0, 0) {
                neighbours.push((dx, dz, resident_column(source, cx + dx, cz + dz)?));
            }
        }
    }
    Some((column, neighbours))
}

pub(super) fn settle_resident_light_snapshot<S: ChunkSource + ?Sized>(
    source: &S,
    cx: i32,
    cz: i32,
    neighbour_offsets: &[(i32, i32)],
    resident_only: bool,
    compute: &mut dyn FnMut(
        &ChunkColumn,
        &[(i32, i32, &ChunkColumn)],
    ) -> Option<lodestone_world::ColumnLight>,
) -> Option<(ChunkColumn, Option<lodestone_world::ColumnLight>)> {
    let _timing = PhaseTimer::start(WorldgenTimingPhase::ResidentLightSettlement, 1);
    let mut fallback = if resident_only {
        resident_column(source, cx, cz)?
    } else {
        source.column(cx, cz)
    };
    for attempt in 0..=LIGHT_SETTLEMENT_OPTIMISTIC_RETRIES {
        let exclusive = attempt == LIGHT_SETTLEMENT_OPTIMISTIC_RETRIES;
        match source.settle_resident_column_light_with_neighbours(
            cx,
            cz,
            &fallback,
            neighbour_offsets,
            resident_only,
            true,
            exclusive,
            compute,
        ) {
            Ok(column) => {
                let light = column.centre_settled_light().cloned();
                return Some((column, light));
            }
            Err(ColumnLightSettlementError::MissingFootprint) => return None,
            Err(ColumnLightSettlementError::NoLight) => return Some((fallback, None)),
            Err(ColumnLightSettlementError::Conflict) if !exclusive => {
                fallback = if resident_only {
                    resident_column(source, cx, cz)?
                } else {
                    source.column(cx, cz)
                };
            }
            Err(ColumnLightSettlementError::Conflict) => return None,
        }
    }
    unreachable!("the bounded resident-light settlement loop always returns")
}

/// Sends one changed column's light from a snapshot that is already resident.
/// Unlike [`send_column_light`], this never generates terrain while running a
/// connection timer. A missing member means the future chunk snapshot, not a
/// live relight, is responsible for carrying the world-tick mutation.
pub(super) async fn send_resident_column_light<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    cx: i32,
    cz: i32,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    let radius = i32::from(proto.uses_cross_column_light());
    let dimension = source
        .dimension()
        .unwrap_or(crate::dimension::Dimension::Overworld);
    let (column, light) = if proto.retains_initial_column_light() {
        let neighbour_offsets = light_neighbour_offsets(proto.uses_cross_column_light());
        let mut compute = |candidate: &ChunkColumn,
                           neighbours: &[(i32, i32, &ChunkColumn)]| {
            let _timing = PhaseTimer::start(WorldgenTimingPhase::ResidentLightCompute, 1);
            if radius != 0 {
                proto.compute_column_light_with_neighbours_in_dimension(
                    candidate,
                    neighbours,
                    dimension,
                )
            } else {
                proto.compute_column_light_in_dimension(candidate, dimension)
            }
        };
        let Some(settled) = settle_resident_light_snapshot(
            source,
            cx,
            cz,
            &neighbour_offsets,
            true,
            &mut compute,
        ) else {
            return Ok(());
        };
        settled
    } else {
        let Some((column, neighbours)) = resident_light_neighbourhood(source, cx, cz, radius) else {
            return Ok(());
        };
        let light = if radius != 0 {
            let neighbour_refs = borrowed_neighbours(&neighbours);
            proto.compute_column_light_with_neighbours_in_dimension(
                &column,
                &neighbour_refs,
                dimension,
            )
        } else {
            proto.compute_column_light_in_dimension(&column, dimension)
        };
        (column, light)
    };
    if let Some(light) = light {
        let directive = {
            let _timing = PhaseTimer::start(WorldgenTimingPhase::ResidentLightEncode, 1);
            proto.encode_light_update(cx, cz, &light)
        };
        if !matches!(directive, ServerDirective::None) {
            apply(conn, state, directive).await?;
            return Ok(());
        }
    }
    // The captured centre keeps the protocol's full-column fallback non-generating.
    apply(conn, state, proto.begin_chunk_batch()).await?;
    let packet_column = column_for_initial_encode(&column);
    let directive = match proto.try_encode_chunk_in_dimension(cx, cz, &packet_column, dimension) {
        Ok(directive) => directive,
        Err(error) => return return_chunk_encode_error(conn, proto, state, Some(0), error).await,
    };
    apply(conn, state, directive).await?;
    apply(conn, state, proto.end_chunk_batch(1)).await?;
    Ok(())
}

pub(super) fn tick_relight_targets(
    changed_columns: impl IntoIterator<Item = (i32, i32)>,
    delivered: &HashSet<(i32, i32)>,
    radius: i32,
) -> HashSet<(i32, i32)> {
    let mut targets = HashSet::new();
    for (cx, cz) in changed_columns {
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                let affected = (cx + dx, cz + dz);
                if delivered.contains(&affected) {
                    targets.insert(affected);
                }
            }
        }
    }
    targets
}

#[derive(Default)]
pub(super) struct PendingRelights {
    pub(super) queued: VecDeque<(i32, i32)>,
    pub(super) queued_set: HashSet<(i32, i32)>,
    pub(super) generation_required: HashSet<(i32, i32)>,
    pub(super) retry_at: Option<lodestone_time::Instant>,
    pub(super) retry_single: bool,
}

pub(super) struct RelightBatch {
    pub(super) coordinates: Vec<(i32, i32)>,
    pub(super) generation_required: Vec<(i32, i32)>,
}

pub(super) enum RelightBatchOutcome {
    Committed(Vec<((i32, i32), lodestone_world::ColumnLight)>),
    Deferred,
    Unsupported,
    Failed(ChunkEncodeError),
}

impl PendingRelights {
    pub(super) fn enqueue_batch(&mut self, positions: impl IntoIterator<Item = (i32, i32)>) -> usize {
        let mut positions = positions.into_iter().collect::<Vec<_>>();
        positions.sort_unstable();
        positions.dedup();
        let mut added = 0;
        for position in positions {
            if self.queued_set.insert(position) {
                self.queued.push_back(position);
                added += 1;
            }
        }
        if added != 0 {
            self.retry_at = None;
        }
        added
    }

    pub(super) fn enqueue_edit(&mut self, cx: i32, cz: i32, radius: i32) {
        self.retry_at = None;
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                let coordinate = (cx + dx, cz + dz);
                if self.queued_set.insert(coordinate) {
                    self.queued.push_back(coordinate);
                }
                self.generation_required.insert(coordinate);
            }
        }
    }

    pub(super) fn pop_front(&mut self) -> Option<(i32, i32)> {
        let position = self.queued.pop_front()?;
        self.queued_set.remove(&position);
        self.generation_required.remove(&position);
        Some(position)
    }

    pub(super) fn requires_generation(&self, coordinate: (i32, i32)) -> bool {
        self.generation_required.contains(&coordinate)
    }

    pub(super) fn front(&self) -> Option<(i32, i32)> {
        self.queued.front().copied()
    }

    pub(super) fn clear(&mut self) {
        self.queued.clear();
        self.queued_set.clear();
        self.generation_required.clear();
        self.retry_at = None;
        self.retry_single = false;
    }

    pub(super) fn ready(&self) -> bool {
        !self.is_empty() && self.retry_at.is_none_or(|at| lodestone_time::Instant::now() >= at)
    }

    pub(super) fn batch(&self, delivered: &HashSet<(i32, i32)>, shared: bool) -> RelightBatch {
        let Some(anchor) = self.front() else {
            return RelightBatch { coordinates: Vec::new(), generation_required: Vec::new() };
        };
        let coordinates = self.queued.iter().copied().filter(|&(cx, cz)| {
            delivered.contains(&(cx, cz)) && (if shared && !self.retry_single {
                (i64::from(cx) - i64::from(anchor.0)).abs() <= 1
                    && (i64::from(cz) - i64::from(anchor.1)).abs() <= 1
            } else {
                (cx, cz) == anchor
            })
        }).take(9).collect::<Vec<_>>();
        let generation_required = coordinates.iter().copied()
            .filter(|&coordinate| self.requires_generation(coordinate)).collect();
        RelightBatch { coordinates, generation_required }
    }

    pub(super) fn admit(&mut self, batch: &RelightBatch) {
        self.retry_single = false;
        self.queued.retain(|coordinate| !batch.coordinates.contains(coordinate));
        for coordinate in &batch.coordinates {
            self.queued_set.remove(coordinate);
            self.generation_required.remove(coordinate);
        }
    }

    pub(super) fn defer(&mut self, batch: &RelightBatch) {
        for &coordinate in batch.coordinates.iter().cycle().skip(1).take(batch.coordinates.len()) {
            if self.queued_set.insert(coordinate) {
                self.queued.push_back(coordinate);
            }
        }
        self.generation_required.extend(batch.generation_required.iter().copied()
            .filter(|coordinate| batch.coordinates.contains(coordinate)));
        self.retry_at = Some(lodestone_time::Instant::now() + Duration::from_millis(50));
        self.retry_single = true;
    }

    pub(super) fn is_empty(&self) -> bool {
        self.queued.is_empty()
    }

    pub(super) fn len(&self) -> usize {
        self.queued.len()
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) struct DetachedRelight {
    pub(super) batch: RelightBatch,
    pub(super) handle: crate::worldgen_dispatch::DispatchHandle<RelightBatchOutcome>,
}

pub(super) fn relight_transaction_outcome(error: crate::chunk::ResidentLightTransactionError) -> RelightBatchOutcome {
    use crate::chunk::ResidentLightTransactionError;
    match error {
        ResidentLightTransactionError::Busy | ResidentLightTransactionError::MissingFootprint
        | ResidentLightTransactionError::Conflict => RelightBatchOutcome::Deferred,
        ResidentLightTransactionError::InvalidOutputs => RelightBatchOutcome::Failed(
            ChunkEncodeError::new("resident lighting rejected an invalid output batch"),
        ),
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn compute_detached_relight_batch(
    source: &dyn ChunkSource,
    coordinates: &[(i32, i32)],
    dimension: crate::dimension::Dimension,
    compute: crate::protocol::ResidentLightBatchCompute,
) -> RelightBatchOutcome {
    let footprint = match lodestone_world::ResidentLightFootprint::new(coordinates.iter().copied()) {
        Ok(footprint) => footprint,
        Err(error) => return RelightBatchOutcome::Failed(ChunkEncodeError::new(format!("resident light footprint: {error:?}"))),
    };
    let transaction = match source.try_begin_resident_light(coordinates, footprint.inputs()) {
        Some(Ok(transaction)) => transaction,
        Some(Err(error)) => return relight_transaction_outcome(error),
        None => return RelightBatchOutcome::Unsupported,
    };
    let lights = match compute(coordinates, transaction.columns(), dimension) {
        Ok(lights) => lights,
        Err(error) => return RelightBatchOutcome::Failed(ChunkEncodeError::new(format!("resident light solve: {error:?}"))),
    };
    match transaction.commit(lights.clone()) {
        Ok(()) => RelightBatchOutcome::Committed(lights),
        Err(error) => relight_transaction_outcome(error),
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) struct CooperativeRelight<'a> {
    pub(super) batch: RelightBatch,
    pub(super) future: std::pin::Pin<Box<dyn std::future::Future<Output = RelightBatchOutcome> + 'a>>,
}

#[cfg(target_arch = "wasm32")]
pub(super) async fn compute_cooperative_relight<P: ServerProtocol, S: ChunkSource + ?Sized>(
    proto: &P,
    source: &S,
    coordinates: Vec<(i32, i32)>,
) -> RelightBatchOutcome {
    let footprint = match lodestone_world::ResidentLightFootprint::new(coordinates.iter().copied()) {
        Ok(footprint) => footprint,
        Err(error) => return RelightBatchOutcome::Failed(ChunkEncodeError::new(format!("resident light footprint: {error:?}"))),
    };
    let transaction = {
        let _timing = PhaseTimer::start(WorldgenTimingPhase::ResidentLightSettlement, coordinates.len() as u32);
        match source.try_begin_resident_light(&coordinates, footprint.inputs()) {
            Some(Ok(transaction)) => transaction,
            Some(Err(error)) => return relight_transaction_outcome(error),
            None => return RelightBatchOutcome::Unsupported,
        }
    };
    let dimension = source.dimension().unwrap_or(crate::dimension::Dimension::Overworld);
    let lights = {
        let Some(compute) = proto.compute_resident_light_batch(&coordinates, transaction.columns(), dimension) else {
            return RelightBatchOutcome::Unsupported;
        };
        match compute.await {
            Ok(lights) => lights,
            Err(error) => return RelightBatchOutcome::Failed(ChunkEncodeError::new(format!("resident light solve: {error:?}"))),
        }
    };
    let _timing = PhaseTimer::start(WorldgenTimingPhase::ResidentLightSettlement, 0);
    match transaction.commit(lights.clone()) {
        Ok(()) => RelightBatchOutcome::Committed(lights),
        Err(error) => relight_transaction_outcome(error),
    }
}

pub(super) async fn send_committed_relights<T: Transport, P: ServerProtocol, S: ChunkSource + ?Sized>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    delivered: &HashSet<(i32, i32)>,
    pending: &mut PendingRelights,
    lights: Vec<((i32, i32), lodestone_world::ColumnLight)>,
) -> Result<(), ServerError> {
    for (coordinate, light) in lights {
        if !delivered.contains(&coordinate) {
            continue;
        }
        let current = match source.try_resident_column(coordinate.0, coordinate.1) {
            Some(crate::chunk_store::TryResident::Present(column)) => Some(column),
            Some(_) => None,
            None => resident_column(source, coordinate.0, coordinate.1),
        };
        if !current.as_ref().is_some_and(|column| column.centre_settled_light() == Some(&light)) {
            pending.enqueue_batch([coordinate]);
            continue;
        }
        let directive = {
            let _timing = PhaseTimer::start(WorldgenTimingPhase::ResidentLightEncode, 1);
            proto.encode_light_update(coordinate.0, coordinate.1, &light)
        };
        if matches!(directive, ServerDirective::None) {
            send_resident_column_light(conn, proto, source, state, coordinate.0, coordinate.1).await?;
        } else {
            apply(conn, state, directive).await?;
        }
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn compute_detached_relight(
    source: &dyn ChunkSource,
    coordinate: (i32, i32),
    dimension: crate::dimension::Dimension,
    cross_column: bool,
    resident_only: bool,
    compute: crate::protocol::DetachedLightCompute,
) -> Option<lodestone_world::ColumnLight> {
    let offsets = light_neighbour_offsets(cross_column);
    let mut calculate = |column: &ChunkColumn, neighbours: &[(i32, i32, &ChunkColumn)]| {
        let _timing = PhaseTimer::start(WorldgenTimingPhase::ResidentLightCompute, 1);
        Some(compute(column, neighbours, dimension))
    };
    settle_resident_light_snapshot(
        source,
        coordinate.0,
        coordinate.1,
        &offsets,
        resident_only,
        &mut calculate,
    )?
    .1
}

pub(super) async fn send_next_relight<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    delivered: &HashSet<(i32, i32)>,
    pending_relights: &mut PendingRelights,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    let Some((cx, cz)) = pending_relights.pop_front() else {
        return Ok(());
    };
    if delivered.contains(&(cx, cz)) {
        crate::worldgen_progress::measure_polls(
            WorldgenTimingPhase::ConnectionRelight,
            std::pin::pin!(send_resident_column_light(conn, proto, source, state, cx, cz)),
        ).await?;
    }
    Ok(())
}

/// Recomputes every column a boundary edit can affect. A light source can cross
/// either seam and a corner, so the correct bounded footprint is the edited
/// column plus all eight neighbours; the light engine's 15-block radius cannot
/// reach beyond that 3×3 footprint.
pub(super) async fn send_lighting_for_edit<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    cx: i32,
    cz: i32,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    let radius = i32::from(proto.uses_cross_column_light());
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            send_column_light(conn, proto, source, state, cx + dx, cz + dz).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
