//! Chunk-column encoding for the wire: light settlement across neighbours, admission footprints, and the initial-column fast path.

use super::*;

/// Encodes one complete chunk through the production source-aware initial-
/// chunk path.
///
/// Protocols that retain initial column light use the source's required
/// settled footprint (the complete three-by-three footprint when cross-column
/// light is enabled) before encoding; protocols without that lifecycle use
/// their ordinary dimension-aware encoder.
/// The supplied `column` is the caller's fallback when the source has no
/// resident centre copy. This is the same seam used by the live join path and
/// by persistence parity harnesses, so callers must provide the source that
/// owns the world lifecycle rather than a detached generator.
pub fn encode_chunk_with_source<P: ServerProtocol>(
    proto: &P,
    source: &dyn ChunkSource,
    cx: i32,
    cz: i32,
    column: &ChunkColumn,
) -> Result<ServerDirective, ChunkEncodeError> {
    encode_chunk_with_source_receipt(proto, source, cx, cz, column)
        .map(|encoded| encoded.directive)
}

pub(super) fn encode_chunk_with_source_receipt<P: ServerProtocol>(
    proto: &P,
    source: &dyn ChunkSource,
    cx: i32,
    cz: i32,
    column: &ChunkColumn,
) -> Result<EncodedColumn, ChunkEncodeError> {
    let dimension = source
        .dimension()
        .unwrap_or(crate::dimension::Dimension::Overworld);
    if !proto.retains_initial_column_light() {
        let packet_column = column_for_initial_encode(column);
        let _timing = PhaseTimer::start(WorldgenTimingPhase::PacketEncoding, 1);
        return proto.try_encode_chunk_in_dimension(cx, cz, &packet_column, dimension)
            .map(|directive| EncodedColumn {
                directive, stage: Some(packet_column.generation_stage()),
            });
    }
    // The initial packet must be based on a complete, settled 3×3 footprint.
    // Prefer the source's current centre copy because another admission may
    // already have installed a newer retained snapshot than the argument held
    // by the streaming queue. The source's settlement transaction validates
    // that this copy remains current after the potentially expensive light
    // computation.
    let neighbour_offsets = light_neighbour_offsets(proto.uses_cross_column_light());
    let mut fallback = resident_column(source, cx, cz).unwrap_or_else(|| column.clone());
    for attempt in 0..=LIGHT_SETTLEMENT_OPTIMISTIC_RETRIES {
        let mut captured_neighbours = Vec::new();
        let exclusive = attempt == LIGHT_SETTLEMENT_OPTIMISTIC_RETRIES;
        let mut compute = |centre: &ChunkColumn, neighbours: &[(i32, i32, &ChunkColumn)]| {
            {
                let _timing = PhaseTimer::start(WorldgenTimingPhase::SnapshotAssembly, 1);
                captured_neighbours = neighbours
                    .iter()
                    .map(|(dx, dz, neighbour)| (*dx, *dz, (*neighbour).clone()))
                    .collect();
            }
            let _timing = PhaseTimer::start(WorldgenTimingPhase::PacketLighting, 1);
            proto.compute_initial_column_lights_with_neighbours_in_dimension(
                centre,
                neighbours,
                dimension,
            )
        };
        match source.settle_resident_column_lights_with_neighbours(
            cx,
            cz,
            &fallback,
            &neighbour_offsets,
            false,
            false,
            exclusive,
            &mut compute,
        ) {
            Ok(centre) => {
                // A persisted centre may already carry a settled light
                // snapshot, so the source transaction can return before the
                // compute callback captures its neighbours. The packet still
                // needs that complete footprint; load it for encoding without
                // replacing the retained centre snapshot.
                if captured_neighbours.is_empty() && !neighbour_offsets.is_empty() {
                    captured_neighbours = neighbour_offsets
                        .iter()
                        .map(|&(dx, dz)| (dx, dz, source.column(cx + dx, cz + dz)))
                        .collect();
                }
                let packet_column = column_for_initial_encode(&centre);
                let neighbour_refs = borrowed_neighbours(&captured_neighbours);
                let _timing = PhaseTimer::start(WorldgenTimingPhase::PacketEncoding, 1);
                return proto.try_encode_chunk_with_neighbours_in_dimension(
                    cx,
                    cz,
                    &packet_column,
                    &neighbour_refs,
                    dimension,
                ).map(|directive| EncodedColumn {
                    directive, stage: Some(packet_column.generation_stage()),
                });
            }
            Err(ColumnLightSettlementError::NoLight) => {
                if captured_neighbours.is_empty() && !neighbour_offsets.is_empty() {
                    captured_neighbours = neighbour_offsets
                        .iter()
                        .map(|&(dx, dz)| (dx, dz, source.column(cx + dx, cz + dz)))
                        .collect();
                }
                let packet_column = column_for_initial_encode(&fallback);
                let neighbour_refs = borrowed_neighbours(&captured_neighbours);
                let _timing = PhaseTimer::start(WorldgenTimingPhase::PacketEncoding, 1);
                return proto.try_encode_chunk_with_neighbours_in_dimension(
                    cx,
                    cz,
                    &packet_column,
                    &neighbour_refs,
                    dimension,
                ).map(|directive| EncodedColumn {
                    directive, stage: Some(packet_column.generation_stage()),
                });
            }
            Err(ColumnLightSettlementError::MissingFootprint) => {
                return Err(ChunkEncodeError::new(
                    "initial retained-light settlement lost its source footprint",
                ));
            }
            Err(ColumnLightSettlementError::Conflict) if !exclusive => {
                // A dependency write completed after capture. Retry from the
                // current source view so the next light snapshot describes the
                // newer blocks rather than the rejected one.
                fallback = resident_column(source, cx, cz).unwrap_or_else(|| column.clone());
            }
            Err(ColumnLightSettlementError::Conflict) => {
                return Err(ChunkEncodeError::new(
                    "initial retained-light settlement remained unstable after the exclusive retry",
                ));
            }
        }
    }
    unreachable!("the bounded initial-light settlement loop always returns")
}

/// Gives a direct initial-packet encoder only a centre-settled retained light
/// snapshot. The source settlement path is the authority for promoting a
/// dependency-initialized column; a `NoLight` result must not let a direct
/// encoder mistake that intermediate storage for the final centre answer.
pub(super) fn column_for_initial_encode(column: &ChunkColumn) -> ChunkColumn {
    let _timing = PhaseTimer::start(WorldgenTimingPhase::SnapshotAssembly, 1);
    let mut column = column.clone();
    if column.retained_light().is_some() && column.centre_settled_light().is_none() {
        column.clear_retained_light();
    }
    column
}

#[cfg(test)]
pub(super) fn detached_initial_packet_columns<P: ServerProtocol>(
    proto: &P,
    column: &ChunkColumn,
    neighbours: &[(i32, i32, &ChunkColumn)],
    dimension: crate::dimension::Dimension,
) -> ChunkColumn {
    let mut column = column_for_initial_encode(column);
    if let Some(settlement) = proto.compute_initial_column_lights_with_neighbours_in_dimension(
        &column,
        neighbours,
        dimension,
    ) {
        column.set_retained_light_with_status(
            settlement.centre_light().clone(),
            crate::chunk::RetainedLightStatus::CentreSettled,
        );
    }
    column
}

pub(super) fn borrowed_neighbours(
    neighbours: &[(i32, i32, ChunkColumn)],
) -> Vec<(i32, i32, &ChunkColumn)> {
    neighbours
        .iter()
        .map(|(dx, dz, column)| (*dx, *dz, column))
        .collect()
}

pub(super) const LIGHT_SETTLEMENT_OPTIMISTIC_RETRIES: usize = 3;

pub(super) fn light_neighbour_offsets(cross_column: bool) -> Vec<(i32, i32)> {
    if !cross_column {
        return Vec::new();
    }
    (-1..=1)
        .flat_map(|dz| (-1..=1).map(move |dx| (dx, dz)))
        .filter(|&(dx, dz)| (dx, dz) != (0, 0))
        .collect()
}

/// Coordinates that a synchronous consumer may touch after admitting one
/// centre. Radius one covers the target column, a face-adjacent placement, and
/// the complete retained-light neighbourhood used by initial packet encoding.
pub(super) fn column_admission_footprint(cx: i32, cz: i32, radius: i32) -> Vec<(i32, i32)> {
    (-radius..=radius)
        .flat_map(|dz| (-radius..=radius).map(move |dx| (cx + dx, cz + dz)))
        .collect()
}

/// The part of [`column_admission_footprint`] still missing after a streaming
/// worker has already produced the centre column.
///
/// Initial retained-light encoding needs the complete three-by-three footprint,
/// but the join pipeline hands this path a fully generated centre. Asking the
/// source for that centre again is redundant work on sources that do not retain
/// generated columns, and a needless resident-cache lookup on sources that do.
/// Preserve the footprint's row-major ordering while excluding only `(cx, cz)`.
pub(super) fn column_admission_neighbours(cx: i32, cz: i32, radius: i32) -> Vec<(i32, i32)> {
    column_admission_footprint(cx, cz, radius)
        .into_iter()
        .filter(|&pos| pos != (cx, cz))
        .collect()
}

/// Returns the block position whose column an inbound world action owns.
///
/// The packet branch awaits this broker before entering its existing
/// synchronous handlers. A packet is never discarded when the source is cold;
/// admission only establishes the ordering that lets the handler run without
/// making a generation call on the connection task.
pub(super) fn action_target(packet: &ServerBound) -> Option<BlockPos> {
    match packet {
        ServerBound::BlockAction { pos, .. }
        | ServerBound::UseItemOn { pos, .. }
        | ServerBound::SetCommandBlock { pos, .. }
        | ServerBound::PickItemFromBlock { pos, .. } => Some(*pos),
        _ => None,
    }
}

/// Admits the owned action footprint before the packet handler reads or
/// mutates terrain. This is intentionally one awaited operation in front of
/// the `match packet`: packet order is therefore preserved even when a cold
/// source takes a worker turn to generate its target and neighbours.
pub(super) async fn admit_action_footprint<S: ChunkSource + 'static>(
    source: SourceRef<'_, S>,
    packet: &ServerBound,
) -> Result<(), ChunkEncodeError> {
    let Some(pos) = action_target(packet) else {
        return Ok(());
    };
    source
        .admit_columns(column_admission_footprint(
            pos.x.div_euclid(16),
            pos.z.div_euclid(16),
            1,
        ))
        .await
}

pub(super) async fn encode_column<P: ServerProtocol, S: ChunkSource + 'static>(
    proto: &P,
    source: SourceRef<'_, S>,
    cx: i32,
    cz: i32,
    trace: Option<&JoinTrace>,
    payload: crate::join_scheduler::ColumnPayload,
) -> Result<EncodedColumn, ChunkEncodeError> {
    let payload = match payload {
        crate::join_scheduler::ColumnPayload::Encoded(directive) => {
            crate::join_scheduler::ColumnPayload::Encoded(directive)
        }
        crate::join_scheduler::ColumnPayload::Column(column) => {
            let column = match source
                .get()
                .packet_generation_stage(column.generation_stage())
            {
                Some(required) if required > column.generation_stage() => {
                    source
                        .generate(vec![(cx, cz)])
                        .await?
                        .into_iter()
                        .next()
                        .ok_or_else(|| ChunkEncodeError::new("packet admission returned no column"))?
                }
                _ => column,
            };
            crate::join_scheduler::ColumnPayload::Column(column)
        }
    };
    if matches!(&payload, crate::join_scheduler::ColumnPayload::Column(_))
        && (proto.uses_cross_column_light() || proto.retains_initial_column_light())
    {
        source
            .admit_columns(column_admission_neighbours(
                cx,
                cz,
                i32::from(proto.uses_cross_column_light()),
            ))
            .await?;
    }
    match payload {
        crate::join_scheduler::ColumnPayload::Encoded(directive) => {
            Ok(EncodedColumn { directive, stage: None })
        }
        crate::join_scheduler::ColumnPayload::Column(column) => {
            let directive = match try_encode_initial_column(
                proto, source.get(), cx, cz, |coordinates| source.generate(coordinates),
            ).await? {
                Some(encoded) => Ok(encoded),
                None => encode_chunk_with_source_receipt(proto, source.get(), cx, cz, &column),
            };
            if directive.is_ok() {
                if let Some(trace) = trace {
                    trace.mark("encoded", cx, cz);
                }
            }
            directive
        }
    }
}

pub(super) async fn try_encode_initial_column<P, F, Fut>(
    proto: &P,
    source: &dyn ChunkSource,
    cx: i32,
    cz: i32,
    admit: F,
) -> Result<Option<EncodedColumn>, ChunkEncodeError>
where
    P: ServerProtocol,
    F: Fn(Vec<(i32, i32)>) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<ChunkColumn>, ChunkEncodeError>>,
{
    use crate::chunk::ResidentLightTransactionError as Error;

    let Some(prepare) = proto.detached_initial_packet_prepare() else {
        return Ok(None);
    };
    let offsets = light_neighbour_offsets(proto.uses_cross_column_light());
    let dimension = source.dimension().unwrap_or(crate::dimension::Dimension::Overworld);
    loop {
        let mut transaction = match source.try_begin_initial_packet(cx, cz, &offsets) {
            None => return Ok(None),
            Some(Ok(transaction)) => transaction,
            Some(Err(Error::Busy | Error::Conflict)) => {
                defer_initial_packet().await;
                continue;
            }
            Some(Err(Error::MissingFootprint)) => {
                admit(column_admission_footprint(
                    cx, cz, i32::from(proto.uses_cross_column_light()),
                )).await?;
                continue;
            }
            Some(Err(Error::InvalidOutputs)) => {
                return Err(ChunkEncodeError::new("invalid initial packet footprint"));
            }
        };
        let input = {
            let _timing = PhaseTimer::start(WorldgenTimingPhase::SnapshotAssembly, 1);
            let columns = transaction.columns();
            let column = columns.iter().find(|(x, z, _)| (*x, *z) == (cx, cz))
                .ok_or_else(|| ChunkEncodeError::new("initial packet capture omitted its centre"))?
                .2.clone();
            let neighbours = offsets.iter().map(|&(dx, dz)| {
                columns.iter().find(|(x, z, _)| (*x, *z) == (cx + dx, cz + dz))
                    .map(|(_, _, column)| (dx, dz, column.clone()))
                    .ok_or_else(|| ChunkEncodeError::new("initial packet capture omitted a dependency"))
            }).collect::<Result<Vec<_>, _>>()?;
            crate::initial_packet::InitialPacketInput {
                coordinate: (cx, cz), dimension, column, neighbours,
            }
        };
        let prepared = crate::join_scheduler::prepare_owned_initial_packet(prepare.clone(), input).await?;
        // The protocol sizes light to the wire dimension, while retained light
        // must match its column's own storage height. A column whose height
        // differs from its wire dimension (a plugin dimension served under a
        // standard dimension's framing) cannot retain that light, so it takes
        // the ordinary encode instead of failing the join.
        if let Some(settlement) = prepared.settlement.as_ref() {
            let retainable = settlement.iter().all(|(offset, light)| {
                transaction.columns().iter()
                    .find(|(x, z, _)| (*x, *z) == (cx + offset.0, cz + offset.1))
                    .is_some_and(|(_, _, column)| light.light_section_count() == column.section_count() + 2)
            });
            if !retainable {
                return Ok(None);
            }
        }
        loop {
            match transaction.try_commit(prepared.settlement.as_ref()) {
                Ok(()) => return Ok(Some(EncodedColumn {
                    directive: prepared.directive, stage: Some(prepared.stage),
                })),
                Err(Error::Busy) => defer_initial_packet().await,
                Err(Error::Conflict | Error::MissingFootprint) => break,
                Err(Error::InvalidOutputs) => {
                    return Err(ChunkEncodeError::new("invalid initial packet light settlement"));
                }
            }
        }
        defer_initial_packet().await;
    }
}

pub(super) async fn defer_initial_packet() {
    #[cfg(target_arch = "wasm32")]
    lodestone_time::browser_yield().await;
    #[cfg(not(target_arch = "wasm32"))]
    tokio::task::yield_now().await;
}

pub(super) async fn encode_column_owned<P: ServerProtocol>(
    proto: &P,
    source: Arc<dyn ChunkSource>,
    cx: i32,
    cz: i32,
    trace: Option<Arc<JoinTrace>>,
    payload: crate::join_scheduler::ColumnPayload,
) -> Result<EncodedColumn, ChunkEncodeError> {
    let payload = match payload {
        crate::join_scheduler::ColumnPayload::Encoded(directive) => {
            crate::join_scheduler::ColumnPayload::Encoded(directive)
        }
        crate::join_scheduler::ColumnPayload::Column(column) => {
            if let Some(encoded) = try_encode_initial_column(
                proto, &*source, cx, cz,
                |coordinates| crate::join_scheduler::generate_owned_columns(Arc::clone(&source), coordinates),
            ).await? {
                if let Some(trace) = trace.as_ref() { trace.mark("encoded", cx, cz); }
                return Ok(encoded);
            }
            let column = match source.packet_generation_stage(column.generation_stage()) {
                Some(required) if required > column.generation_stage() => crate::join_scheduler::generate_owned_columns(
                    Arc::clone(&source),
                    vec![(cx, cz)],
                )
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| ChunkEncodeError::new("packet admission returned no column"))?,
                _ => column,
            };
            crate::join_scheduler::ColumnPayload::Column(column)
        }
    };
    if matches!(&payload, crate::join_scheduler::ColumnPayload::Column(_))
        && (proto.uses_cross_column_light() || proto.retains_initial_column_light())
    {
        let _ = crate::join_scheduler::generate_owned_columns(
            Arc::clone(&source),
            column_admission_neighbours(
                cx,
                cz,
                i32::from(proto.uses_cross_column_light()),
            ),
        )
        .await?;
    }
    match payload {
        crate::join_scheduler::ColumnPayload::Encoded(directive) => {
            Ok(EncodedColumn { directive, stage: None })
        }
        crate::join_scheduler::ColumnPayload::Column(column) => {
            #[cfg(not(target_arch = "wasm32"))]
            let directive = if let Some(encode) = proto.detached_source_encode() {
                let handle = crate::worldgen_dispatch::spawn(move || {
                    encode(&*source, cx, cz, &column)
                })
                .await;
                handle.await.map_err(|_| {
                    ChunkEncodeError::new("detached source encode worker ended without a result")
                })?.map(|directive| EncodedColumn { directive, stage: None })
            } else {
                encode_chunk_with_source_receipt(proto, &*source, cx, cz, &column)
            };
            #[cfg(target_arch = "wasm32")]
            let directive = encode_chunk_with_source_receipt(proto, &*source, cx, cz, &column);
            if directive.is_ok()
                && let Some(trace) = trace.as_ref()
            {
                trace.mark("encoded", cx, cz);
            }
            directive
        }
    }
}
