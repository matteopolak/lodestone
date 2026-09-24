//! Conversion from a frame snapshot to typed render inputs.
//!
//! These functions are deliberately world-free. [`super::snapshot`] has
//! already validated state identity and sampled light, so each renderer can
//! filter and resolve its own appearance without taking another world lock.

use super::{
    bell_spawn, chest_spawn, copper_golem_statue_spawn, is_enchanting_table, lectern_spawn,
    shulker_spawn, BellShakes, BlockEntityFrameSnapshot, ChestLids, ConduitTicks,
    EnchantingTableBooks,
};
use lodestone_render::{
    BellSpawn, ChestSpawn, ConduitSpawn, CopperGolemStatueSpawn, EnchantingTableSpawn,
    LecternSpawn, ShulkerSpawn,
};

#[must_use]
pub(crate) fn chest_spawns_from_snapshot(
    snapshot: &BlockEntityFrameSnapshot,
    lids: &ChestLids,
    partial_tick: f32,
) -> Vec<ChestSpawn> {
    let mut out = Vec::new();
    for candidate in &snapshot.candidates {
        if let Some(spawn) = chest_spawn(
            candidate.pos,
            candidate.state_id,
            lids.openness(candidate.pos, partial_tick),
            candidate.light,
        ) {
            out.push(spawn);
        }
    }
    out.sort_by_key(|s| s.pos);
    out
}

#[must_use]
pub(crate) fn bell_spawns_from_snapshot(
    snapshot: &BlockEntityFrameSnapshot,
    shakes: &BellShakes,
    partial_tick: f32,
) -> Vec<BellSpawn> {
    let mut out = Vec::new();
    for candidate in &snapshot.candidates {
        if let Some(spawn) = bell_spawn(
            candidate.pos,
            candidate.state_id,
            candidate.light,
            shakes,
            partial_tick,
        ) {
            out.push(spawn);
        }
    }
    out.sort_by_key(|s| s.pos);
    out
}

#[must_use]
pub(crate) fn shulker_spawns_from_snapshot(
    snapshot: &BlockEntityFrameSnapshot,
) -> Vec<ShulkerSpawn> {
    let mut out = Vec::new();
    for candidate in &snapshot.candidates {
        if let Some(spawn) = shulker_spawn(candidate.pos, candidate.state_id, candidate.light) {
            out.push(spawn);
        }
    }
    out.sort_by_key(|s| s.pos);
    out
}

#[must_use]
pub(crate) fn lectern_spawns_from_snapshot(
    snapshot: &BlockEntityFrameSnapshot,
) -> Vec<LecternSpawn> {
    let mut out = Vec::new();
    for candidate in &snapshot.candidates {
        if let Some(spawn) = lectern_spawn(candidate.pos, candidate.state_id, candidate.light) {
            out.push(spawn);
        }
    }
    out.sort_by_key(|s| s.pos);
    out
}

#[must_use]
pub(crate) fn enchanting_table_spawns_from_snapshot(
    snapshot: &BlockEntityFrameSnapshot,
    books: &EnchantingTableBooks,
    partial_tick: f32,
) -> Vec<EnchantingTableSpawn> {
    let mut out = Vec::new();
    for candidate in &snapshot.candidates {
        if !is_enchanting_table(candidate.state_id.raw()) {
            continue;
        }
        let (y_rot, time, open, flip) = books
            .state(candidate.pos, partial_tick)
            .unwrap_or_default();
        let spawn = EnchantingTableSpawn {
            pos: candidate.pos,
            y_rot,
            time,
            open,
            flip,
            light: candidate.light,
        };
        out.push(spawn);
    }
    out.sort_by_key(|s| s.pos);
    out
}

#[must_use]
pub(crate) fn conduit_spawns_from_snapshot(
    snapshot: &BlockEntityFrameSnapshot,
    ticks: &ConduitTicks,
    partial_tick: f32,
) -> Vec<ConduitSpawn> {
    let mut out = Vec::new();
    for candidate in &snapshot.candidates {
        if candidate.state_id.name() != "minecraft:conduit" {
            continue;
        }
        if let Some(spawn) = ticks.resolve(candidate.pos, partial_tick, candidate.light) {
            out.push(spawn);
        }
    }
    out.sort_by_key(|s| s.pos);
    out
}

#[must_use]
pub(crate) fn copper_golem_statue_spawns_from_snapshot(
    snapshot: &BlockEntityFrameSnapshot,
) -> Vec<CopperGolemStatueSpawn> {
    let mut out = Vec::new();
    for candidate in &snapshot.candidates {
        if let Some(spawn) = copper_golem_statue_spawn(
            candidate.pos,
            candidate.state_id.raw(),
            candidate.light,
        ) {
            out.push(spawn);
        }
    }
    out.sort_by_key(|s| s.pos);
    out
}
