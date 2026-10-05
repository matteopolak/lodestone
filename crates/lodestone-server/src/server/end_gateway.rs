//! End-gateway contact rules: when a body may use a gateway and where it comes out.

use super::*;

/// Resolves the generated End-gateway block entity at the player's contact
/// cell. The live registry is authoritative for loaded chunks; the source
/// fallback is required for generator-backed sources whose column has not yet
/// been hydrated into a registry. Checking the block state first prevents a
/// stale sidecar from teleporting through a gateway block that was removed.
#[cfg(test)]
pub(super) fn end_gateway_destination<S: ChunkSource + ?Sized>(
    source: &S,
    block_entities: &BlockEntityHandle,
    pos: BlockPos,
) -> Option<Vec3> {
    if !crate::portal::is_end_gateway(source.block_state_id(pos.x, pos.y, pos.z)) {
        return None;
    }
    let configured = block_entities
        .with(|registry| registry.get(pos).and_then(BlockEntity::gateway_destination))
        .or_else(|| source.block_entity(pos.x, pos.y, pos.z).and_then(|entity| entity.gateway_destination()));
    configured.and_then(|(exit, exact)| {
        crate::portal::end_gateway_arrival_in_world(source, exit, exact)
    })
}

/// Reads the gateway sidecar after the contact column is known resident. The
/// exit is returned separately so the async connection path can admit the
/// bounded arrival footprint before the resident-only search runs.
pub(super) fn end_gateway_exit_resident<S: ChunkSource + ?Sized>(
    source: &S,
    block_entities: &BlockEntityHandle,
    pos: BlockPos,
) -> Option<(BlockPos, bool)> {
    let state = resident_block_state(source, pos.x, pos.y, pos.z)?;
    if !crate::portal::is_end_gateway(state) {
        return None;
    }
    block_entities
        .with(|registry| registry.get(pos).and_then(BlockEntity::gateway_destination))
        .or_else(|| {
            resident_column(source, pos.x.div_euclid(16), pos.z.div_euclid(16))
                .and_then(|column| column.block_entities().iter()
                    .find(|(at, _)| *at == pos)
                    .and_then(|(_, entity)| entity.gateway_destination()))
        })
}

pub(super) fn end_gateway_contact_allowed(dimension: crate::dimension::Dimension, is_player: bool) -> bool {
    dimension == crate::dimension::Dimension::End && is_player
}

pub(super) fn player_has_mount(mobs: &MobHandle, player_entity_id: i32) -> bool {
    mobs.with(|sim| {
        sim.vehicle_ridden_by(player_entity_id).is_some()
            || sim.minecart_ridden_by(player_entity_id).is_some()
            || sim.mob_ridden_by(player_entity_id).is_some()
    })
}

pub(super) const END_GATEWAY_CONTACT_COOLDOWN: u8 = 40;

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct EndGatewayTeleport {
    pub(super) position: Vec3,
    pub(super) dimension: crate::dimension::Dimension,
    pub(super) cooldown: u8,
}

#[cfg(test)]
pub(super) fn resolve_end_gateway_contact<S: ChunkSource + ?Sized>(
    source: &S,
    block_entities: &BlockEntityHandle,
    pos: BlockPos,
    dimension: crate::dimension::Dimension,
    cooldown: u8,
    is_player: bool,
    mounted: bool,
) -> Option<EndGatewayTeleport> {
    if cooldown != 0 || mounted || !end_gateway_contact_allowed(dimension, is_player) {
        return None;
    }
    end_gateway_destination(source, block_entities, pos).map(|position| EndGatewayTeleport {
        position,
        dimension,
        cooldown: END_GATEWAY_CONTACT_COOLDOWN,
    })
}
