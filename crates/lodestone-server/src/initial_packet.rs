use crate::chunk::{ChunkColumn, ChunkGenerationStage, ColumnLightSettlement};
use crate::dimension::Dimension;
use crate::protocol::{ChunkEncodeError, ServerDirective, ServerProtocol};
use crate::worldgen_progress::{PhaseTimer, WorldgenTimingPhase};

#[derive(Debug)]
pub struct InitialPacketInput {
    pub coordinate: (i32, i32),
    pub dimension: Dimension,
    pub column: ChunkColumn,
    pub neighbours: Vec<(i32, i32, ChunkColumn)>,
}

#[derive(Debug)]
pub struct PreparedInitialPacket {
    pub directive: ServerDirective,
    pub stage: ChunkGenerationStage,
    pub settlement: Option<ColumnLightSettlement>,
}

pub fn prepare_initial_packet_with_protocol<P: ServerProtocol>(
    proto: &P,
    input: InitialPacketInput,
) -> Result<PreparedInitialPacket, ChunkEncodeError> {
    let InitialPacketInput { coordinate: (cx, cz), dimension, mut column, neighbours } = input;
    let neighbours: Vec<_> = neighbours.iter()
        .map(|(dx, dz, column)| (*dx, *dz, column)).collect();
    let settlement = if proto.retains_initial_column_light()
        && column.centre_settled_light().is_none()
    {
        let _timing = PhaseTimer::start(WorldgenTimingPhase::PacketLighting, 1);
        proto.compute_initial_column_lights_with_neighbours_in_dimension(
            &column, &neighbours, dimension,
        )
    } else {
        None
    };
    if let Some(settlement) = &settlement {
        column.set_retained_light(settlement.centre_light().clone());
    } else if column.centre_settled_light().is_none() {
        column.clear_retained_light();
    }
    let _timing = PhaseTimer::start(WorldgenTimingPhase::PacketEncoding, 1);
    let directive = if proto.retains_initial_column_light() {
        proto.try_encode_chunk_with_neighbours_in_dimension(cx, cz, &column, &neighbours, dimension)
    } else {
        proto.try_encode_chunk_in_dimension(cx, cz, &column, dimension)
    }?;
    Ok(PreparedInitialPacket { directive, stage: column.generation_stage(), settlement })
}
