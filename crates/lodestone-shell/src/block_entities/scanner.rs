//! World scanners that produce debug/render candidates from block entities.
//!
//! These scans own chunk-world access and distance/permission filtering. They
//! return render-neutral data or debug vertices; render-input conversion stays
//! in the parent facade so existing callers keep the same paths.

use glam::Vec3;
use lodestone_world::{ChunkPos, World};

use crate::{
    gpu::{DebugLineVertex, push_box},
    net::SharedHandle,
};

const STRUCTURE_BLOCK_VIEW_DISTANCE: f32 = 96.0;
const STRUCTURE_BOX_COLOR: [f32; 4] = [0.9, 0.9, 0.9, 1.0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StructureBox {
    pub(super) min: [i32; 3],
    pub(super) max: [i32; 3],
}

fn nbt_int_field(fields: &[(String, lodestone_core::Nbt)], key: &str) -> Option<Option<i32>> {
    match fields.iter().find(|(name, _)| name == key).map(|(_, value)| value) {
        Some(lodestone_core::Nbt::Int(value)) => Some(Some(*value)),
        None => Some(None),
        Some(_) => None,
    }
}

fn nbt_string_field<'a>(fields: &'a [(String, lodestone_core::Nbt)], key: &str) -> Option<Option<&'a str>> {
    match fields.iter().find(|(name, _)| name == key).map(|(_, value)| value) {
        Some(lodestone_core::Nbt::String(value)) => Some(Some(value)),
        None => Some(None),
        Some(_) => None,
    }
}

fn nbt_bool_field(fields: &[(String, lodestone_core::Nbt)], key: &str) -> Option<Option<bool>> {
    match fields.iter().find(|(name, _)| name == key).map(|(_, value)| value) {
        Some(lodestone_core::Nbt::Byte(value)) => Some(Some(*value != 0)),
        None => Some(None),
        Some(_) => None,
    }
}

pub(super) fn structure_box(block: [i32; 3], nbt: &lodestone_core::Nbt) -> Option<StructureBox> {
    let lodestone_core::Nbt::Compound(fields) = nbt else { return None };
    let mode = nbt_string_field(fields, "mode")?.unwrap_or("DATA");
    let show_bounding_box = nbt_bool_field(fields, "showboundingbox")?.unwrap_or(true);
    if mode != "SAVE" && (mode != "LOAD" || !show_bounding_box) { return None }

    let origin = [
        nbt_int_field(fields, "posX")?.unwrap_or(0).clamp(-48, 48),
        nbt_int_field(fields, "posY")?.unwrap_or(1).clamp(-48, 48),
        nbt_int_field(fields, "posZ")?.unwrap_or(0).clamp(-48, 48),
    ];
    let size = [
        nbt_int_field(fields, "sizeX")?.unwrap_or(0).clamp(0, 48),
        nbt_int_field(fields, "sizeY")?.unwrap_or(0).clamp(0, 48),
        nbt_int_field(fields, "sizeZ")?.unwrap_or(0).clamp(0, 48),
    ];
    if size.iter().any(|axis| *axis < 1) { return None }

    let (x_diff, z_diff) = match nbt_string_field(fields, "mirror")?.unwrap_or("NONE") {
        "LEFT_RIGHT" => (size[0], -size[2]),
        "FRONT_BACK" => (-size[0], size[2]),
        _ => (size[0], size[2]),
    };
    let (x0, z0, x1, z1) = match nbt_string_field(fields, "rotation")?.unwrap_or("NONE") {
        "CLOCKWISE_90" => {
            let x0 = if z_diff < 0 { origin[0] } else { origin[0] + 1 };
            let z0 = if x_diff < 0 { origin[2] + 1 } else { origin[2] };
            (x0, z0, x0 - z_diff, z0 + x_diff)
        }
        "CLOCKWISE_180" => {
            let x0 = if x_diff < 0 { origin[0] } else { origin[0] + 1 };
            let z0 = if z_diff < 0 { origin[2] } else { origin[2] + 1 };
            (x0, z0, x0 - x_diff, z0 - z_diff)
        }
        "COUNTERCLOCKWISE_90" => {
            let x0 = if z_diff < 0 { origin[0] + 1 } else { origin[0] };
            let z0 = if x_diff < 0 { origin[2] } else { origin[2] + 1 };
            (x0, z0, x0 + z_diff, z0 - x_diff)
        }
        _ => {
            let x0 = if x_diff < 0 { origin[0] + 1 } else { origin[0] };
            let z0 = if z_diff < 0 { origin[2] + 1 } else { origin[2] };
            (x0, z0, x0 + x_diff, z0 + z_diff)
        }
    };

    Some(StructureBox {
        min: [block[0] + x0.min(x1), block[1] + origin[1], block[2] + z0.min(z1)],
        max: [block[0] + x0.max(x1), block[1] + origin[1] + size[1], block[2] + z0.max(z1)],
    })
}

#[must_use]
pub(crate) fn can_render_structure_boxes(permission_level: u8, instabuild: bool, spectator: bool) -> bool {
    spectator || (instabuild && permission_level >= 2)
}

#[must_use]
pub(crate) fn structure_block_outline_vertices(block: [i32; 3], nbt: &lodestone_core::Nbt) -> Vec<DebugLineVertex> {
    let Some(bounds) = structure_box(block, nbt) else { return Vec::new() };
    let mut vertices = Vec::with_capacity(24);
    push_box(&mut vertices, bounds.min.map(|axis| axis as f32), bounds.max.map(|axis| axis as f32), STRUCTURE_BOX_COLOR);
    vertices
}

#[must_use]
pub(crate) fn structure_block_vertices_from_loaded_world(
    world: &World,
    chunks: impl IntoIterator<Item = ChunkPos>,
    eye: Vec3,
    permission_level: u8,
    instabuild: bool,
    spectator: bool,
) -> Vec<DebugLineVertex> {
    if !can_render_structure_boxes(permission_level, instabuild, spectator) { return Vec::new() }
    let cutoff = STRUCTURE_BLOCK_VIEW_DISTANCE * STRUCTURE_BLOCK_VIEW_DISTANCE;
    let mut vertices = Vec::new();
    for chunk_pos in chunks {
        let Some(chunk) = world.get(chunk_pos) else { continue };
        for entity in &chunk.block_entities {
            let block = [
                chunk_pos.x * 16 + i32::from(entity.rel_x),
                i32::from(entity.y),
                chunk_pos.z * 16 + i32::from(entity.rel_z),
            ];
            let centre = Vec3::new(block[0] as f32 + 0.5, block[1] as f32 + 0.5, block[2] as f32 + 0.5);
            if centre.distance_squared(eye) >= cutoff { continue }
            let state_id = chunk.column.get_block(usize::from(entity.rel_x), block[1], usize::from(entity.rel_z));
            if lodestone_data::block_states::block_name(state_id) != Some("minecraft:structure_block") { continue }
            vertices.extend(structure_block_outline_vertices(block, &entity.nbt));
        }
    }
    vertices
}

#[must_use]
pub(crate) fn structure_block_vertices(
    handle: &SharedHandle,
    eye: Vec3,
    permission_level: u8,
    instabuild: bool,
    spectator: bool,
) -> Vec<DebugLineVertex> {
    let Some(client) = handle.get() else { return Vec::new() };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();
    let world = store.read();
    structure_block_vertices_from_loaded_world(
        &world,
        chunks.into_iter().map(|chunk| ChunkPos { x: chunk.x, z: chunk.z }),
        eye,
        permission_level,
        instabuild,
        spectator,
    )
}
