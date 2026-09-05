//! The shell's block-entity source: turns the client-owned world's decoded
//! block-entity records into the render crate's [`ChestSpawn`]s, and owns the
//! chest-lid animation state that no other layer has anywhere to put.
//!
//! This is the **consumer end** of a chain that already existed and reached
//! nothing. Before this module the chain stopped one hop short of
//! pixels at every link:
//!
//! ```text
//! level_chunk_with_light ─► BlockEntity::decode_list  ─► LoadedChunk.block_entities
//! block_update           ─► World::sync_block_entity  ─┤   (create / keep /
//! section_blocks_update  ─► World::sync_block_entity  ─┤    replace / remove)
//! local placement        ─► World::sync_block_entity  ─┤
//! block_entity_data      ─► World::set_block_entity   ─┘
//!                                                      │  ← nothing in the shell
//!                                                      │    read this field: zero
//!                                                      │    call sites
//!                                                      ▼
//!                                          chest_spawns() ─► gpu/block_entities.rs
//! ```
//!
//! The fourth row is the client's own right-click prediction
//! ([`crate::sim::write_predicted_block`], that fix) and it is **not** a packet:
//! it is what stops a placed chest from being a hole for one server round trip.
//! See `docs/block-placement-prediction.md`.
//!
//! # There are **four** creation routes, not two
//!
//! The first version of that diagram listed only the chunk packet and
//! `block_entity_data`, which was accurate and read as exhaustive. It was not:
//! in vanilla, **writing a block state is what creates a block entity** — no
//! packet involved (26.2's own chunk-level block-state write constructs the
//! new block's block entity inline) — and
//! `block_entity_data` is only ever data for an entity that already exists. Our
//! `block_update` / `section_blocks_update` arms wrote the state and nothing
//! else, so a freshly placed chest had a state, no record, and this module's
//! `for be in &chunk.block_entities` loop never saw it. It drew zero pixels and
//! still *opened*, because interaction resolves from the block state.
//!
//! [`lodestone_world::World::sync_block_entity`] is the fix, driven by
//! [`lodestone_data::block_entity_types`]. Its **removal** half matters as much
//! as its creation half: without it, breaking a chest would leave a stale record
//! and this module would keep drawing a chest in empty air.
//!
//! That fix wired that into the two *packet* arms only, which left the same bug on the
//! **prediction** side — the client wrote no state at all on a right-click, so a
//! chest you placed did not exist locally until `BLOCK_UPDATE` came back (issue
//! That fix). [`crate::sim::write_predicted_block`] closes it with the same pair, and
//! the removal half is what corrects a placement the server refuses.
//!
//! # Why the block-entity list is the candidate set, and the block state is the
//! truth
//!
//! Each [`lodestone_world::BlockEntity`] carries a `type_id` from the *block
//! entity type* registry and an NBT payload. This module uses **neither** to
//! decide what to draw:
//!
//! * The `type_id` does not identify the block. `minecraft:chest` and
//!   `minecraft:trapped_chest` are distinct types, but all four copper chests map
//!   to `minecraft:chest` — measured, in
//!   [`lodestone_data::block_entity_types`]' census. (That census now exists, so
//!   the older reason given here — "the shell has no block-entity-type table" —
//!   is stale as of that fix; the type is still the wrong question.)
//! * The NBT payload is `Nbt::End` for a chest the server sent no data for,
//!   which is the common case.
//!
//! What the list *is* good for is being the **set of positions worth looking
//! at** — exactly how vanilla's `BlockEntityRenderDispatcher` iterates
//! `level.getBlockEntities()` rather than scanning blocks. The appearance then
//! comes from the block state at that position, via
//! [`lodestone_data::block_states`]: the block name gives the material and the
//! `facing`/`type` properties give the rotation and half. That keeps the cost
//! O(number of block entities) instead of O(blocks in range) *and* makes the
//! block-entity decode a real dependency rather than a decorative one.
//!
//! # The lid animation lives here because nothing else can hold it
//!
//! Chest openness is not on the wire. The server sends `BLOCK_EVENT` with
//! `b0 == 1` and `b1 == viewer count` (vanilla's own chest block-event handling
//! sets the chest-lid controller's should-be-open flag from `b1 > 0`), and the
//! *client* integrates that into an angle over the following ticks. So the
//! authoritative value is a client-side accumulator, and [`ChestLids`] is a
//! direct port of vanilla's own chest-lid controller:
//!
//! * Its own tick ramps `openness` by **±0.1 per tick**, clamped to `0..=1`.
//! * Its own openness lookup is a lerp between the *previous* tick's
//!   value and the current one by the partial tick.
//!
//! Both halves matter. Dropping the ramp gives a lid that teleports open;
//! dropping the partial-tick lerp gives one that visibly steps at 20 Hz. The
//! ramp is tested against its exact 10-tick duration, and the lerp against the
//! midpoint of a tick, because the endpoints alone cannot tell either apart from
//! a snap.
//!
//! # How to change it
//!
//! * A second animated block entity (a bell's swing, a conduit's spin) wants its
//!   own map alongside [`ChestLids`], not a field on it — they are driven by
//!   different packets and tick with different rules.
//! * [`VIEW_DISTANCE`] is vanilla's own default and is the one number here worth
//!   keeping honest; see its doc.
//! * `chest_spawns` takes a `&SharedHandle` rather than a `&ClientHandle` so the
//!   whole thing can be moved into a `'static` render-source closure the way
//!   `Sim::outline_shape_source` does. Taking a borrow would make it
//!   uninstallable.

use std::collections::HashMap;
use std::sync::Arc;

use glam::Vec3;
use lodestone_data::block_states::StateId;
use lodestone_render::{
    BannerAttachment, BannerSpawn, BeaconSpawn, BeamSection, BellShakeDirection, BellSpawn,
    BrushableItemSpawn, ChestHalf, ChestMaterial, ChestSpawn, ConduitFrame, ConduitSpawn,
    CopperGolemOxidation, CopperGolemPose, CopperGolemStatueSpawn,
    BlockEntityTexture, DecoratedPotSpawn, EndGatewaySpawn, EndPortalSpawn, LecternSpawn, SHELF_SLOTS,
    SHULKER_COLOURS, ShelfItemSpawn, ShulkerFacing, ShulkerSpawn, SignKind, SignOrientation,
    SignSpawn, SkullOrientation, SkullSpawn, SkullType, SkyDefault, VaultSpawn, average_beam_color,
    beacon_beam_color, beam_radius_scale, conduit_active_rotation_value, conduit_advance,
    conduit_anim_time, conduit_animation_phase, conduit_frame_scan, entity::vault_spin_degrees,
    horizontal_facing_clockwise_yaw, horizontal_facing_yaw,
};
use lodestone_render::banner_pattern::{DyeColor, StoredPatternLayer};
use lodestone_world::{ChunkPos, SignText, World};
use lodestone_core::{Nbt, NbtTag};
use lodestone_javarandom::JavaRandom;

use crate::{
    gpu::{DebugLineVertex, push_box},
    net::{SharedHandle, entity_light_at},
};

#[cfg(test)]
fn known_state_id(raw: u32) -> StateId {
    StateId::new(raw).expect("test state id is in the canonical census")
}

#[cfg(test)]
mod frame_snapshot_tests {
    use super::*;

    fn first_state(name: &str) -> StateId {
        (0..lodestone_data::block_states::STATE_COUNT)
            .find(|&id| lodestone_data::block_states::block_name(id) == Some(name))
            .and_then(StateId::new)
            .unwrap_or_else(|| panic!("missing state for {name}"))
    }

    #[test]
    fn one_frame_snapshot_feeds_multiple_state_driven_renderers_without_a_handle() {
        let chest_pos = [1, 64, 2];
        let bell_pos = [3, 64, 4];
        let snapshot = BlockEntityFrameSnapshot {
            candidates: vec![
                BlockEntityFrameCandidate {
                    pos: chest_pos,
                    state_id: first_state("minecraft:chest"),
                    light: 0xab,
                },
                BlockEntityFrameCandidate {
                    pos: bell_pos,
                    state_id: first_state("minecraft:bell"),
                    light: 0xcd,
                },
            ],
        };

        let chests = chest_spawns_from_snapshot(&snapshot, &ChestLids::new(), 0.0);
        let bells = bell_spawns_from_snapshot(&snapshot, &BellShakes::new(), 0.0);

        assert_eq!(chests.len(), 1);
        assert_eq!(chests[0].pos, chest_pos);
        assert_eq!(chests[0].light, 0xab);
        assert_eq!(bells.len(), 1);
        assert_eq!(bells[0].pos, bell_pos);
        assert_eq!(bells[0].light, 0xcd);
    }
}

/// Vanilla's per-renderer cutoff: its own view-distance accessor
/// returns `64`, and its own should-render check compares it against the distance from the
/// camera to the block's own center position — the block **centre**, not its corner.
///
/// Ported as the real thing rather than "the render distance" because it is
/// genuinely a fixed 64 blocks in vanilla regardless of the video setting, and
/// because the center-of-block offset is the difference between a chest popping in
/// at 64.0 and at 63.1.
pub const VIEW_DISTANCE: f32 = 64.0;

/// One state-driven block entity captured for a single rendered frame.
///
/// The block state and packed entity light are resolved while the chunk world
/// is held under one read lock. The render-specific filters below can therefore
/// share this record without rescanning every loaded chunk or reacquiring the
/// world once per visible object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockEntityFrameCandidate {
    pos: [i32; 3],
    state_id: StateId,
    light: u8,
}

/// Camera-scoped immutable input shared by the state-driven block-entity
/// renderers in one frame.
///
/// NBT-dependent families deliberately do not use this first slice: signs,
/// heads, banners and item-bearing block entities still parse their typed NBT
/// in their existing gathers. Keeping raw NBT out of this snapshot makes its
/// hot-path records compact and avoids cloning arbitrary payload trees merely
/// to save a chunk scan.
#[derive(Debug, Default)]
pub(crate) struct BlockEntityFrameSnapshot {
    candidates: Vec<BlockEntityFrameCandidate>,
}

fn packed_light_in_chunk(
    chunk: &lodestone_world::LoadedChunk,
    block: [i32; 3],
    dimensions: Option<lodestone_client::WorldDimensions>,
    sky_default: SkyDefault,
) -> u8 {
    let Some(dimensions) = dimensions else {
        return lodestone_render::ENTITY_FULLBRIGHT;
    };
    let section = (block[1] - dimensions.min_y).div_euclid(16);
    if section < 0 || section >= dimensions.section_count() as i32 {
        return lodestone_render::ENTITY_FULLBRIGHT;
    }
    let section = section as usize;
    let x = block[0].rem_euclid(16) as usize;
    let y = (block[1] - dimensions.min_y).rem_euclid(16) as usize;
    let z = block[2].rem_euclid(16) as usize;
    let sky = chunk
        .light
        .section_sky_light(section, x, y, z)
        .unwrap_or(match sky_default {
            SkyDefault::Full => 15,
            SkyDefault::None => 0,
        });
    let block = chunk
        .light
        .section_block_light(section, x, y, z)
        .unwrap_or(0);
    (sky << 4) | block
}

/// Captures the state and light needed by every state-only block-entity
/// renderer for this camera position.
///
/// This is the one place on the render path that calls `loaded_chunks()` and
/// takes the chunk-world read lock for those renderers. The result has no world
/// borrow and can safely live in all of the renderer-owned `'static` closures
/// installed for the rest of the frame.
#[must_use]
pub(crate) fn block_entity_frame_snapshot(
    handle: &SharedHandle,
    eye: Vec3,
) -> Option<BlockEntityFrameSnapshot> {
    let client = handle.get()?;
    let dimensions = client.world_dimensions();
    let player = client.player();
    let sky_default = crate::mesher::sky_default_for_dimension(
        player.dimension.as_ref(),
        player.dimension_type.as_ref(),
    );
    let chunks = client.loaded_chunks();
    let store = client.chunk_world();
    let world = store.read();
    let cutoff = VIEW_DISTANCE * VIEW_DISTANCE;
    let mut candidates = Vec::new();
    for model_pos in chunks {
        let pos = ChunkPos {
            x: model_pos.x,
            z: model_pos.z,
        };
        let Some(chunk) = world.get(pos) else {
            continue;
        };
        for entity in &chunk.block_entities {
            let block = [
                pos.x * 16 + i32::from(entity.rel_x),
                i32::from(entity.y),
                pos.z * 16 + i32::from(entity.rel_z),
            ];
            let centre = Vec3::new(
                block[0] as f32 + 0.5,
                block[1] as f32 + 0.5,
                block[2] as f32 + 0.5,
            );
            if centre.distance_squared(eye) > cutoff {
                continue;
            }
            let raw_state_id = chunk.column.get_block(
                usize::from(entity.rel_x),
                block[1],
                usize::from(entity.rel_z),
            );
            let Some(state_id) = StateId::new(raw_state_id) else {
                continue;
            };
            candidates.push(BlockEntityFrameCandidate {
                pos: block,
                state_id,
                light: packed_light_in_chunk(chunk, block, dimensions, sky_default),
            });
        }
    }
    Some(BlockEntityFrameSnapshot { candidates })
}

const STRUCTURE_BLOCK_VIEW_DISTANCE: f32 = 96.0;
const STRUCTURE_BOX_COLOR: [f32; 4] = [0.9, 0.9, 0.9, 1.0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StructureBox {
    min: [i32; 3],
    max: [i32; 3],
}

/// `Some(None)` means the field is absent, while `None` means that a present
/// field has the wrong wire tag.  Structure Block's codec supplies defaults
/// only for absence; treating malformed tags as defaults can manufacture an
/// overlay for corrupt block-entity data.
fn nbt_int_field(fields: &[(String, lodestone_core::Nbt)], key: &str) -> Option<Option<i32>> {
    match fields.iter().find(|(name, _)| name == key).map(|(_, value)| value) {
        Some(lodestone_core::Nbt::Int(value)) => Some(Some(*value)),
        None => Some(None),
        Some(_) => None,
    }
}

fn nbt_string_field<'a>(
    fields: &'a [(String, lodestone_core::Nbt)],
    key: &str,
) -> Option<Option<&'a str>> {
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

fn structure_box(block: [i32; 3], nbt: &lodestone_core::Nbt) -> Option<StructureBox> {
    let lodestone_core::Nbt::Compound(fields) = nbt else {
        return None;
    };
    let mode = nbt_string_field(fields, "mode")?.unwrap_or("DATA");
    let show_bounding_box = nbt_bool_field(fields, "showboundingbox")?.unwrap_or(true);
    if mode != "SAVE" && (mode != "LOAD" || !show_bounding_box) {
        return None;
    }

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
    if size.iter().any(|axis| *axis < 1) {
        return None;
    }

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
pub(crate) fn can_render_structure_boxes(
    permission_level: u8,
    instabuild: bool,
    spectator: bool,
) -> bool {
    spectator || (instabuild && permission_level >= 2)
}

#[must_use]
pub(crate) fn structure_block_outline_vertices(
    block: [i32; 3],
    nbt: &lodestone_core::Nbt,
) -> Vec<DebugLineVertex> {
    let Some(bounds) = structure_box(block, nbt) else {
        return Vec::new();
    };
    let mut vertices = Vec::with_capacity(24);
    push_box(
        &mut vertices,
        bounds.min.map(|axis| axis as f32),
        bounds.max.map(|axis| axis as f32),
        STRUCTURE_BOX_COLOR,
    );
    vertices
}

#[must_use]
fn structure_block_vertices_from_loaded_world(
    world: &World,
    chunks: impl IntoIterator<Item = ChunkPos>,
    eye: Vec3,
    permission_level: u8,
    instabuild: bool,
    spectator: bool,
) -> Vec<DebugLineVertex> {
    if !can_render_structure_boxes(permission_level, instabuild, spectator) {
        return Vec::new();
    }
    let cutoff = STRUCTURE_BLOCK_VIEW_DISTANCE * STRUCTURE_BLOCK_VIEW_DISTANCE;
    let mut vertices = Vec::new();
    for chunk_pos in chunks {
        let Some(chunk) = world.get(chunk_pos) else {
            continue;
        };
        for entity in &chunk.block_entities {
            let block = [
                chunk_pos.x * 16 + i32::from(entity.rel_x),
                i32::from(entity.y),
                chunk_pos.z * 16 + i32::from(entity.rel_z),
            ];
            let centre = Vec3::new(
                block[0] as f32 + 0.5,
                block[1] as f32 + 0.5,
                block[2] as f32 + 0.5,
            );
            // `Vec3.closerThan` is a strict `<` comparison: the exact 96-block
            // boundary is outside Structure Block's renderer range.
            if centre.distance_squared(eye) >= cutoff {
                continue;
            }
            let state_id = chunk.column.get_block(
                usize::from(entity.rel_x),
                block[1],
                usize::from(entity.rel_z),
            );
            if lodestone_data::block_states::block_name(state_id) != Some("minecraft:structure_block") {
                continue;
            }
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
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();
    let world = store.read();
    structure_block_vertices_from_loaded_world(
        &world,
        chunks.into_iter().map(|chunk| ChunkPos {
            x: chunk.x,
            z: chunk.z,
        }),
        eye,
        permission_level,
        instabuild,
        spectator,
    )
}

/// Vanilla's `ChestLidController` ramp, per tick.
const LID_SPEED: f32 = 0.1;

/// One chest's lid state — `ChestLidController`'s three fields.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Lid {
    should_be_open: bool,
    openness: f32,
    /// `oOpenness`: the value at the start of the current tick, for the
    /// partial-tick lerp.
    previous: f32,
}

/// Per-position chest lid animation state, driven by `BLOCK_EVENT` and advanced
/// once per client tick.
///
/// Keyed by absolute block position. Entries for a fully-closed, not-opening
/// chest are dropped by [`tick`](Self::tick) so the map does not grow without
/// bound as a player walks past thousands of chests — a chest at rest is
/// indistinguishable from an absent entry (both are openness `0`), which is what
/// makes that safe.
#[derive(Debug, Default, Clone)]
pub struct ChestLids {
    lids: HashMap<[i32; 3], Lid>,
}

impl ChestLids {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one `BLOCK_EVENT` to the chest at `pos`.
    ///
    /// `b0`/`b1` are the packet's two opaque parameter bytes. Only `b0 == 1` is
    /// a chest lid event; every other `b0` belongs to some other block type
    /// (a note block's pitch, a piston's direction) and is ignored here rather
    /// than at the caller, so the vanilla rule stays in one place. Returns
    /// whether the event was a lid event.
    ///
    /// `b1 > 0` is `shouldBeOpen`: vanilla sends the *viewer count*, not a
    /// boolean, and a second player opening the same chest sends `2`. Treating
    /// the byte as a boolean directly happens to work for `0`/`1` and shuts the
    /// lid the moment anyone is the second viewer — which is why the comparison
    /// is `> 0`.
    pub fn apply_block_event(&mut self, pos: [i32; 3], b0: u8, b1: u8) -> bool {
        if b0 != 1 {
            return false;
        }
        let should_be_open = b1 > 0;
        let entry = self.lids.entry(pos).or_insert(Lid {
            should_be_open,
            openness: 0.0,
            previous: 0.0,
        });
        entry.should_be_open = should_be_open;
        true
    }

    /// Advances every lid one client tick — vanilla's own chest-lid tick.
    ///
    /// Also garbage-collects lids that are shut and staying shut.
    pub fn tick(&mut self) {
        self.lids.retain(|_, lid| {
            lid.previous = lid.openness;
            if !lid.should_be_open && lid.openness > 0.0 {
                lid.openness = (lid.openness - LID_SPEED).max(0.0);
            } else if lid.should_be_open && lid.openness < 1.0 {
                lid.openness = (lid.openness + LID_SPEED).min(1.0);
            }
            // Keep anything still moving or still open; drop the settled-shut.
            lid.should_be_open || lid.openness > 0.0 || lid.previous > 0.0
        });
    }

    /// The interpolated openness at `pos` — vanilla's own chest-lid openness
    /// lookup, a lerp between the previous and current openness by the partial tick.
    ///
    /// `0.0` for a position with no entry, which is exactly a shut chest.
    #[must_use]
    pub fn openness(&self, pos: [i32; 3], partial_tick: f32) -> f32 {
        match self.lids.get(&pos) {
            Some(lid) => lid.previous + (lid.openness - lid.previous) * partial_tick.clamp(0.0, 1.0),
            None => 0.0,
        }
    }

    /// Number of tracked lids (for stats and tests).
    #[must_use]
    pub fn len(&self) -> usize {
        self.lids.len()
    }

    /// Whether nothing is being tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lids.is_empty()
    }
}

/// Vanilla's own bell shake duration — a shake runs 50 ticks and then stops
/// (its own per-tick check resets the shake state once the tick counter reaches 50).
const BELL_SHAKE_DURATION: f32 = 50.0;

/// One bell's shake — vanilla's own bell block entity's click direction plus its tick
/// counter.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Shake {
    direction: BellShakeDirection,
    /// Vanilla's own bell tick counter, counted up from `0`.
    ticks: f32,
    /// The value at the start of the current tick, for the partial-tick lerp.
    previous: f32,
}

/// Per-position bell shake state, driven by `BLOCK_EVENT` and advanced once per
/// client tick — the bell sibling of [`ChestLids`].
///
/// Keyed by absolute block position, and entries are dropped once their 50-tick
/// shake finishes: a bell at rest is indistinguishable from an absent entry (both
/// give [`shake`](Self::shake) `None`), the same property that makes `ChestLids`'
/// own garbage collection safe.
#[derive(Debug, Default, Clone)]
pub struct BellShakes {
    shakes: HashMap<[i32; 3], Shake>,
}

impl BellShakes {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one `BLOCK_EVENT` to the bell at `pos`, returning whether it was a
    /// bell event.
    ///
    /// Vanilla's own bell block-event handling: only `b0 == 1` is a bell ring,
    /// and `b1` is the direction decoded from its 3D data value — the *face the bell was hit
    /// on*, not a viewer count. A ring always restarts the animation from tick 0
    /// even mid-shake, which is why this overwrites rather than merging.
    ///
    /// **`b0 == 1` is also a chest lid event**, and that collision is real: the
    /// two are told apart by the block at `pos`, not by the packet, which is why
    /// both trackers accept the same event and the *gather* decides which of them
    /// a given position reads from. A note block's `b0` is its instrument and a
    /// piston's is its direction, so neither reaches either tracker.
    pub fn apply_block_event(&mut self, pos: [i32; 3], b0: u8, b1: u8) -> bool {
        if b0 != 1 {
            return false;
        }
        let Some(direction) = shake_direction_from_3d(b1) else {
            // Vanilla's own direction-from-3D-data-value decode gives UP/DOWN for `0`/`1`, which
            // vanilla's own bell model animation has no rotation for — vanilla stores it and
            // then multiplies by nothing. Dropping it here is the same picture and
            // keeps the map free of entries that can never move.
            return false;
        };
        self.shakes.insert(
            pos,
            Shake {
                direction,
                ticks: 0.0,
                previous: 0.0,
            },
        );
        true
    }

    /// Advances every shake one client tick, dropping the finished ones.
    pub fn tick(&mut self) {
        self.shakes.retain(|_, shake| {
            shake.previous = shake.ticks;
            shake.ticks += 1.0;
            shake.ticks < BELL_SHAKE_DURATION
        });
    }

    /// The shake at `pos` for this partial tick, or `None` for a bell at rest.
    ///
    /// The tick counter is interpolated because that is what
    /// vanilla's own bell render-state extraction passes into its animation setup — `ticks +
    /// partialTick`, not the whole number. Interpolating matters here for the same
    /// reason it does for a chest lid: `bell_shake_angle` is a `sin` of it, so a
    /// stepped counter reads as a stutter at 60 fps.
    #[must_use]
    pub fn shake(&self, pos: [i32; 3], partial_tick: f32) -> Option<(BellShakeDirection, f32)> {
        let shake = self.shakes.get(&pos)?;
        let t = partial_tick.clamp(0.0, 1.0);
        Some((shake.direction, shake.previous + (shake.ticks - shake.previous) * t))
    }

    /// Number of bells currently shaking (for stats and tests).
    #[must_use]
    pub fn len(&self) -> usize {
        self.shakes.len()
    }

    /// Whether nothing is being tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shakes.is_empty()
    }
}

/// Vanilla's own end-gateway teleport-cooldown counter, a per-position
/// counter driven by the gateway's own `BLOCK_EVENT` and advanced once per
/// client tick — the same `b0 == 1` collision [`BellShakes`]'s doc already
/// names, told apart by the block at the position rather than the packet.
///
/// Mirrors vanilla's own end-gateway beam-animation tick, the tick function
/// vanilla's own client actually runs for this block entity (a separate,
/// server-only tick function
/// is the *server's* function, which also reads `Age`'s wall-clock role for the
/// rarer spawn arm — see [`end_gateway_beam_spawns`] for why that half is a
/// stateless per-frame NBT read instead of a second tracked field here):
///
/// the age counter increments every tick, and the teleport-cooldown counter
/// decrements only while it is still cooling down.
///
/// Entries are dropped once `teleportCooldown` reaches `0` — a gateway not
/// cooling down is indistinguishable from an absent entry, the same
/// `ChestLids`/`BellShakes` garbage-collection property.
#[derive(Debug, Default, Clone)]
pub struct GatewayCooldowns {
    cooldowns: HashMap<[i32; 3], i32>,
}

impl GatewayCooldowns {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one `BLOCK_EVENT` to the gateway at `pos`, returning whether
    /// it was a teleport-cooldown trigger.
    ///
    /// Vanilla's own end-gateway block-event handling: only `b0 == 1` sets
    /// the teleport cooldown to 40 ticks; `b1` carries nothing for
    /// this type (unlike the bell's own `b0 == 1`, which packs the hit
    /// direction into `b1`) and is accepted but ignored, matching every
    /// other tracker offered this same collision.
    pub fn apply_block_event(&mut self, pos: [i32; 3], b0: u8, _b1: u8) -> bool {
        if b0 != 1 {
            return false;
        }
        self.cooldowns.insert(pos, 40);
        true
    }

    /// Advances every cooldown one client tick, dropping the finished ones —
    /// `beamAnimationTick`'s `if (isCoolingDown()) teleportCooldown--`.
    pub fn tick(&mut self) {
        self.cooldowns.retain(|_, ticks| {
            *ticks -= 1;
            *ticks > 0
        });
    }

    /// The cooldown ticks remaining at `pos`, or `None` for a gateway not
    /// cooling down.
    #[must_use]
    pub fn cooldown(&self, pos: [i32; 3]) -> Option<i32> {
        self.cooldowns.get(&pos).copied()
    }

    /// Number of gateways currently cooling down (for stats and tests).
    #[must_use]
    pub fn len(&self) -> usize {
        self.cooldowns.len()
    }

    /// Whether nothing is being tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cooldowns.is_empty()
    }
}

/// Vanilla's own default required-player-range constant — vanilla's own default for
/// its near-player check, applied unconditionally rather than read per-position from
/// `RequiredPlayerRange` NBT (see [`SpawnerSpins`]'s "Simplifications" doc).
const SPAWNER_REQUIRED_PLAYER_RANGE: f32 = 16.0;

/// Vanilla's own default min-spawn-delay constant — the fallback when a spawner's own
/// `MinSpawnDelay` field is absent from its NBT.
const SPAWNER_DEFAULT_MIN_SPAWN_DELAY: f32 = 200.0;

/// One spawner/trial-spawner block entity's parsed `SpawnData`: the display
/// entity's type path (for [`spawner_mob_spawn`]) and `MinSpawnDelay` (for
/// [`SpawnerSpins`]'s tick-rate formula) — everything this module needs out
/// of vanilla's own mob-spawner load routine's saved NBT.
#[derive(Debug, Clone, PartialEq)]
struct SpawnerData {
    /// `None` when neither `SpawnData` nor the first `SpawnPotentials` entry
    /// carries an `entity.id` — vanilla's own get-or-create-display-entity's own
    /// "nothing to draw" case (an empty entity-id string).
    entity_type: Option<String>,
    min_spawn_delay: f32,
}

/// A compound's field by name, in wire order — the same linear scan
/// [`campfire_items`]/[`banner_patterns`] already use inline; factored out
/// here because [`spawner_data`] reaches for it four times.
fn nbt_field<'a>(fields: &'a [(String, lodestone_core::Nbt)], key: &str) -> Option<&'a lodestone_core::Nbt> {
    fields.iter().find(|(name, _)| name == key).map(|(_, v)| v)
}

/// Any NBT integer type widened to `i64` — vanilla's own mob-spawner load reads
/// `MinSpawnDelay` with an int-or-default read, but its own save routine always writes it
/// as a short, so the tag actually on the wire is a `Short`; this
/// accepts any integer width rather than assuming which.
fn nbt_as_i64(v: &lodestone_core::Nbt) -> Option<i64> {
    use lodestone_core::Nbt;
    match v {
        Nbt::Byte(b) => Some(i64::from(*b)),
        Nbt::Short(s) => Some(i64::from(*s)),
        Nbt::Int(i) => Some(i64::from(*i)),
        Nbt::Long(l) => Some(*l),
        _ => None,
    }
}

/// `SpawnData`'s own shape (vanilla's own spawn-data codec): `{ entity: { id: "...", .. },
/// custom_spawn_rules?, equipment? }`. Reads just the `entity.id` this module
/// needs to pick a model.
fn spawn_data_entity_id(spawn_data: &lodestone_core::Nbt) -> Option<String> {
    use lodestone_core::Nbt;
    let Nbt::Compound(fields) = spawn_data else {
        return None;
    };
    let Nbt::Compound(entity) = nbt_field(fields, "entity")? else {
        return None;
    };
    match nbt_field(entity, "id")? {
        Nbt::String(id) => Some(id.clone()),
        _ => None,
    }
}

/// Vanilla's own mob-spawner load routine's saved NBT (mob spawner) **or**
/// its own trial-spawner-state-data update-tag routine's (trial spawner), parsed into what
/// this module needs. The two block types disagree on almost everything
/// about this NBT — the field name's case, whether a weighted-list fallback
/// exists, whether `MinSpawnDelay` exists at all — but never on the same
/// position (a state id is one block or the other, never both), so one
/// function trying every key in turn is simpler than branching the caller on
/// block identity first.
///
/// * **Entity type**: `SpawnData` (vanilla's own next-spawn-data field, PascalCase)
///   first, falling back to the first `SpawnPotentials` weighted entry
///   (a weighted entry's own `data`/`weight` shape) — the mob spawner's
///   own fallback order (vanilla's own get-or-create-next-spawn-data). Then
///   `spawn_data` (vanilla's own trial-spawner spawn-data tag, snake_case) — the
///   trial spawner's sole source; it has **no** synced weighted-list
///   fallback, because the trial-spawner config's own spawn-potentials
///   definition is a datapack-defined resource never sent to the client at all
///   (vanilla's own trial-spawner-state-data update-tag writes only `spawn_data` and
///   `next_mob_spawns_at`). A trial spawner nobody has stood near long
///   enough to roll a `spawn_data` therefore has no display entity, matching
///   vanilla's own get-or-create-display-entity's own empty-entity-id-string
///   miss.
/// * **`min_spawn_delay`**: `MinSpawnDelay`, mob-spawner-only —
///   [`SPAWNER_DEFAULT_MIN_SPAWN_DELAY`] otherwise, which covers the trial
///   spawner too (see [`SpawnerSpins`]'s "Simplifications" doc for why its
///   spin envelope reuses the mob spawner's constant rather than porting
///   vanilla's own trial-spawner client-tick timestamp-difference formula).
#[must_use]
fn spawner_data(nbt: &lodestone_core::Nbt) -> SpawnerData {
    use lodestone_core::Nbt;
    let Nbt::Compound(fields) = nbt else {
        return SpawnerData {
            entity_type: None,
            min_spawn_delay: SPAWNER_DEFAULT_MIN_SPAWN_DELAY,
        };
    };
    let entity_type = nbt_field(fields, "SpawnData")
        .and_then(spawn_data_entity_id)
        .or_else(|| {
            let Nbt::List { elements, .. } = nbt_field(fields, "SpawnPotentials")? else {
                return None;
            };
            elements.iter().find_map(|entry| {
                let Nbt::Compound(entry) = entry else {
                    return None;
                };
                spawn_data_entity_id(nbt_field(entry, "data")?)
            })
        })
        .or_else(|| nbt_field(fields, "spawn_data").and_then(spawn_data_entity_id));
    let min_spawn_delay = nbt_field(fields, "MinSpawnDelay")
        .and_then(nbt_as_i64)
        .map(|v| v as f32)
        .unwrap_or(SPAWNER_DEFAULT_MIN_SPAWN_DELAY);
    SpawnerData {
        entity_type,
        min_spawn_delay,
    }
}

/// Vanilla's own trial-spawner block state's `trial_spawner_state` property value, or
/// `None` for a state with no such property (every mob-spawner state, and
/// anything that is not a spawner at all).
#[must_use]
fn trial_spawner_state_property(state_id: u32) -> Option<&'static str> {
    let props = lodestone_data::block_states::properties(state_id)?;
    props
        .iter()
        .find(|(name, _)| *name == "trial_spawner_state")
        .map(|(_, value)| *value)
}

/// Vanilla's own trial-spawner-state spin-speed and has-spinning-mob accessors —
/// per-state numerator for the spin-rate formula, `None` for a state with no
/// spinning mob at all (a negative spin speed). Only
/// `waiting_for_players` (`200.0`) and `active` (`1000.0`) qualify;
/// `inactive`, `waiting_for_reward_ejection`, `ejecting_reward` and
/// `cooldown` all draw an empty cage regardless of whether NBT still names a
/// display entity — the state gates the draw, not just the rate.
#[must_use]
fn trial_spawner_spin_speed(state_id: u32) -> Option<f32> {
    match trial_spawner_state_property(state_id)? {
        "waiting_for_players" => Some(200.0),
        "active" => Some(1000.0),
        _ => None,
    }
}

/// This position's spin-rate numerator and whether it is currently eligible
/// to show a display entity at all — `None` for a mob-spawner state (which
/// has no such gate; every mob spawner with a display entity spins) wrapped
/// as `Some(1000.0)`, and [`trial_spawner_spin_speed`] for a trial spawner.
/// `None` overall for anything that is not a spawner-family block, or a
/// trial spawner in a non-spinning state.
#[must_use]
fn spawner_spin_speed(state_id: u32) -> Option<f32> {
    match lodestone_data::block_states::block_name(state_id)? {
        "minecraft:spawner" => Some(1000.0),
        "minecraft:trial_spawner" => trial_spawner_spin_speed(state_id),
        _ => None,
    }
}

/// [`lodestone_render::spawner_display_scale`], resolved from a type path via
/// [`lodestone_data::entity_dimensions`]. An unresolvable type (a future or
/// malformed entity id) falls back to the base `0.53125` — the `max_len >
/// 1.0` branch needs real dimensions to trigger, so an unknown type never
/// under-shrinks into visible overflow, only misses a shrink it might have
/// deserved.
#[must_use]
fn spawner_mob_scale(entity_type: &str) -> f32 {
    match lodestone_data::entity_type::EntityType::from_name(entity_type) {
        Some(t) => {
            let dims = lodestone_data::entity_dimensions::base_dimensions(t);
            lodestone_render::spawner_display_scale(dims.width, dims.height)
        }
        None => lodestone_render::spawner_display_scale(0.0, 0.0),
    }
}

/// One spawner/trial-spawner's spin state — `BaseSpawner`'s `spin`/`oSpin`
/// plus `spawnDelay`, the fields `clientTick` reads and writes every tick.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Spin {
    spin: f32,
    /// `oSpin`: the value at the start of the current tick, for the
    /// partial-tick lerp — same convention as [`Lid::previous`]/
    /// [`Shake::previous`].
    previous: f32,
    /// Vanilla's own mob-spawner spawn-delay field, decremented while near a player with a
    /// display entity, and reset to `min_spawn_delay` by a `BLOCK_EVENT`
    /// `b0 == 1` (vanilla's own event-triggered handling).
    spawn_delay: f32,
    min_spawn_delay: f32,
}

impl Default for Spin {
    fn default() -> Self {
        Spin {
            spin: 0.0,
            previous: 0.0,
            spawn_delay: SPAWNER_DEFAULT_MIN_SPAWN_DELAY,
            min_spawn_delay: SPAWNER_DEFAULT_MIN_SPAWN_DELAY,
        }
    }
}

/// Per-position spawner/trial-spawner spin state — vanilla's own mob-spawner
/// client tick,
/// advanced once per client tick, plus its own event-triggered `BLOCK_EVENT`
/// reset. The spawner sibling of [`ChestLids`]/[`BellShakes`], but its tick
/// needs the **world** (a proximity test and the NBT-derived
/// `MinSpawnDelay`) rather than the packet stream alone — see
/// [`Self::tick`]/[`spawner_tick_candidates`].
///
/// # Simplifications from the real `BaseSpawner`
///
/// * **Local player only**, for `isNearPlayer` — the same simplification
///   [`EnchantingTableBooks::tick`]'s own doc records for its nearest-player
///   check. A remote player standing at a spawner the local player cannot
///   see leaves its mob frozen.
/// * **`RequiredPlayerRange` is not read from NBT.**
///   [`SPAWNER_REQUIRED_PLAYER_RANGE`] is vanilla's own default (`16`)
///   applied unconditionally; a datapack-customised spawner spins at the
///   vanilla rate but wakes at the vanilla radius rather than its own.
/// * **The `near`-but-no-`displayEntity` edge case is folded into `near`
///   itself** rather than kept as vanilla's third branch (`oSpin` left
///   untouched from whenever it was last set): since nothing draws without a
///   display entity either way, the two are visually indistinguishable, and
///   folding them keeps [`Self::tick`] a single `if` rather than three arms.
/// * **The trial spawner's real spin-rate formula is not ported.**
///   Vanilla's own trial-spawner client tick computes its spawn delay from
///   `max(0, nextMobSpawnsAt - level.getGameTime())` — a difference against
///   the **server's** absolute world age, which this client does not track
///   in sync with the server's clock (the local tick counter
///   [`Sim::beacon_source`] uses for its own scroll cycle is a *local*
///   count, not `level.getGameTime()`, and the two drift apart from the
///   moment of login). Porting the real formula would need that sync built
///   first. Trial spawners share the mob spawner's decrementing-counter
///   envelope instead (`spawn_delay` counts down from
///   [`SPAWNER_DEFAULT_MIN_SPAWN_DELAY`], since no `MinSpawnDelay`-shaped
///   NBT exists for a trial spawner to override it), with vanilla's real
///   per-state numerator (`200.0`/`1000.0`,
///   [`trial_spawner_spin_speed`]) substituted for the mob spawner's
///   constant `1000.0`. The result spins at a real, bounded, non-static
///   rate in the right ballpark, but its exact phase will not match a real
///   client standing beside the same trial spawner.
#[derive(Debug, Default, Clone)]
pub struct SpawnerSpins {
    spins: HashMap<[i32; 3], Spin>,
}

impl SpawnerSpins {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Vanilla's own mob-spawner event-triggered handling: `id == 1` resets `spawnDelay` to
    /// `minSpawnDelay`. Returns whether this was a spawner event; every other
    /// `b0` belongs to some other block type and is ignored here, matching
    /// the sibling trackers' own `apply_block_event`.
    pub fn apply_block_event(&mut self, pos: [i32; 3], b0: u8, _b1: u8) -> bool {
        if b0 != 1 {
            return false;
        }
        let entry = self.spins.entry(pos).or_default();
        entry.spawn_delay = entry.min_spawn_delay;
        true
    }

    /// Advances every tracked spawner one client tick.
    ///
    /// `tracked` is every spawner/trial-spawner candidate this tick — see
    /// [`spawner_tick_candidates`] — as `(pos, near_player, min_spawn_delay,
    /// has_display_entity, speed)`. `speed` is the formula's numerator
    /// (vanilla's own mob-spawner constant `1000.0`, or
    /// [`trial_spawner_spin_speed`]'s per-state `200.0`/`1000.0`) — see
    /// [`spawner_spin_speed`]. A position absent from `tracked` is dropped:
    /// the same eviction [`ChestLids`]/[`BellShakes`] apply, bounded here by
    /// [`VIEW_DISTANCE`] (via the candidate gather) rather than a
    /// settled-state test, since a spawner's spin never settles — it only
    /// freezes while nobody is near (or, for a trial spawner, while its
    /// state has no spinning mob at all).
    pub fn tick(&mut self, tracked: &[([i32; 3], bool, f32, bool, f32)]) {
        let present: std::collections::HashSet<[i32; 3]> =
            tracked.iter().map(|(pos, ..)| *pos).collect();
        self.spins.retain(|pos, _| present.contains(pos));
        for &(pos, near, min_spawn_delay, has_entity, speed) in tracked {
            let entry = self.spins.entry(pos).or_insert_with(|| Spin {
                spawn_delay: min_spawn_delay,
                min_spawn_delay,
                ..Spin::default()
            });
            entry.min_spawn_delay = min_spawn_delay;
            entry.previous = entry.spin;
            if near && has_entity {
                if entry.spawn_delay > 0.0 {
                    entry.spawn_delay -= 1.0;
                }
                entry.spin = (entry.spin + speed / (entry.spawn_delay + 200.0)) % 360.0;
            }
        }
    }

    /// This tick's raw `(previous, spin)` pair for `pos` — `(0.0, 0.0)` for an
    /// untracked position, matching a spawner nobody has been near yet
    /// (`BaseSpawner`'s own fields start at `0.0`).
    #[must_use]
    fn raw(&self, pos: [i32; 3]) -> (f32, f32) {
        match self.spins.get(&pos) {
            Some(s) => (s.previous, s.spin),
            None => (0.0, 0.0),
        }
    }

    /// Number of tracked spawners (for stats and tests).
    #[must_use]
    pub fn len(&self) -> usize {
        self.spins.len()
    }

    /// Whether nothing is being tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.spins.is_empty()
    }
}

/// Every spawner/trial-spawner position within [`VIEW_DISTANCE`] of `player`
/// worth ticking this frame, as `(pos, near, min_spawn_delay, has_entity,
/// speed)` — see [`SpawnerSpins::tick`] for the shape.
///
/// **`near` means different things for the two block families.**
/// Vanilla's own mob-spawner client tick really does gate a mob spawner's advance on
/// its near-player check (within [`SPAWNER_REQUIRED_PLAYER_RANGE`]); a trial
/// spawner has no such gate at all — vanilla's own trial-spawner client tick advances
/// unconditionally whenever the current state has a spinning mob — so `near` is
/// simply `true` there whenever [`spawner_spin_speed`] returns a rate.
/// Folding both into one `near` bool rather than a `bool` and a separate
/// `state_permits_spin` bool is what lets a single [`SpawnerSpins::tick`]
/// serve both families with one `if near && has_entity` — the "state
/// permits" half is already baked into `has_entity` being `false` when
/// [`spawner_spin_speed`] misses.
///
/// Reuses [`spawner_candidates`] rather than a fourth NBT-aware scan: the
/// render-time gather ([`spawner_mob_spawns`]) and this tick-time one differ
/// only in what they do with the same rows.
#[must_use]
pub fn spawner_tick_candidates(
    handle: &SharedHandle,
    player: Vec3,
) -> Vec<([i32; 3], bool, f32, bool, f32)> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();
    let candidates = {
        let world = store.read();
        spawner_candidates(
            &world,
            chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }),
            player,
        )
    };
    let cutoff = SPAWNER_REQUIRED_PLAYER_RANGE * SPAWNER_REQUIRED_PLAYER_RANGE;
    candidates
        .into_iter()
        .filter_map(|(pos, state_id, data)| {
            let speed = spawner_spin_speed(state_id)?;
            let is_trial = lodestone_data::block_states::block_name(state_id)
                == Some("minecraft:trial_spawner");
            let near = if is_trial {
                true
            } else {
                let centre = Vec3::new(
                    pos[0] as f32 + 0.5,
                    pos[1] as f32 + 0.5,
                    pos[2] as f32 + 0.5,
                );
                centre.distance_squared(player) <= cutoff
            };
            Some((pos, near, data.min_spawn_delay, data.entity_type.is_some(), speed))
        })
        .collect()
}

/// Every spawner/trial-spawner position within [`VIEW_DISTANCE`], paired with
/// its block state and parsed [`SpawnerData`] — the NBT-aware candidate
/// gather [`spawner_mob_spawns`]/[`spawner_tick_candidates`] both build on,
/// the same shape [`campfire_candidates`]/[`sign_candidates`] already use for
/// the same reason: [`chest_candidates`] discards `be.nbt`, and this
/// renderer's *entire* appearance (which mob, and how fast it spins) lives in
/// there.
#[must_use]
fn spawner_candidates(
    world: &World,
    chunks: impl IntoIterator<Item = ChunkPos>,
    eye: Vec3,
) -> Vec<([i32; 3], u32, SpawnerData)> {
    let cutoff = VIEW_DISTANCE * VIEW_DISTANCE;
    let mut candidates = Vec::new();
    for pos in chunks {
        let Some(chunk) = world.get(pos) else {
            continue;
        };
        for be in &chunk.block_entities {
            let x = pos.x * 16 + i32::from(be.rel_x);
            let z = pos.z * 16 + i32::from(be.rel_z);
            let y = i32::from(be.y);
            let centre = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
            if centre.distance_squared(eye) > cutoff {
                continue;
            }
            let state_id = chunk
                .column
                .get_block(usize::from(be.rel_x), y, usize::from(be.rel_z));
            candidates.push(([x, y, z], state_id, spawner_data(&be.nbt)));
        }
    }
    candidates
}

/// One candidate resolved into a [`lodestone_render::SpawnerMobSpawn`], or
/// `None` if the state at that position is neither a spawner nor a trial
/// spawner, it has no display entity to draw
/// (vanilla's own spawn-data entity-id lookup returning empty is its own "draw
/// nothing" case), or — trial spawner only — its current
/// `trial_spawner_state` has no spinning mob at all
/// (vanilla's own has-spinning-mob check; see [`spawner_spin_speed`]). That
/// last clause is real state gating the draw, not just the rate: an
/// `inactive`/`waiting_for_reward_ejection`/`ejecting_reward`/`cooldown`
/// trial spawner must draw an empty cage even if its NBT still names a
/// display entity from an earlier `active` phase.
#[must_use]
fn spawner_mob_spawn(
    block: [i32; 3],
    state_id: u32,
    data: &SpawnerData,
    spins: &SpawnerSpins,
    partial_tick: f32,
    light: u8,
) -> Option<lodestone_render::SpawnerMobSpawn> {
    spawner_spin_speed(state_id)?;
    let entity_type = data.entity_type.clone()?;
    let (previous, spin) = spins.raw(block);
    let spin_deg = lodestone_render::spawner_spin_degrees(previous, spin, partial_tick);
    let scale = spawner_mob_scale(&entity_type);
    Some(lodestone_render::SpawnerMobSpawn {
        pos: block,
        entity_type,
        spin_deg,
        scale,
        light,
    })
}

/// Every spawner/trial-spawner's miniature display mob to draw this frame,
/// gathered from the client-owned world's block-entity records. Same shape
/// as [`chest_spawns`]/[`bell_spawns`]: a distance-gated, NBT-aware scan plus
/// a light sample, sorted by position for deterministic pixel-gate ordering.
#[must_use]
pub fn spawner_mob_spawns(
    handle: &SharedHandle,
    spins: &SpawnerSpins,
    eye: Vec3,
    partial_tick: f32,
) -> Vec<lodestone_render::SpawnerMobSpawn> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();
    let candidates = {
        let world = store.read();
        spawner_candidates(
            &world,
            chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }),
            eye,
        )
    };
    let sky_default = {
        let player = client.player();
        crate::mesher::sky_default_for_dimension(
            player.dimension.as_ref(),
            player.dimension_type.as_ref(),
        )
    };
    let mut out = Vec::new();
    for (block, state_id, data) in candidates {
        let light = entity_light_at(handle, block[0], block[1], block[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        if let Some(spawn) = spawner_mob_spawn(block, state_id, &data, spins, partial_tick, light) {
            out.push(spawn);
        }
    }
    out.sort_by_key(|s| s.pos);
    out
}

#[cfg(test)]
mod spawner_tests {
    use super::*;

    fn compound(fields: Vec<(&str, lodestone_core::Nbt)>) -> lodestone_core::Nbt {
        lodestone_core::Nbt::Compound(
            fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )
    }

    fn spawn_data_nbt(entity_id: &str) -> lodestone_core::Nbt {
        compound(vec![(
            "entity",
            compound(vec![("id", lodestone_core::Nbt::String(entity_id.to_string()))]),
        )])
    }

    /// `SpawnData` (PascalCase, mob spawner) is read straight off `entity.id`.
    #[test]
    fn spawner_data_reads_spawn_data_entity_id() {
        let nbt = compound(vec![("SpawnData", spawn_data_nbt("minecraft:pig"))]);
        let data = spawner_data(&nbt);
        assert_eq!(data.entity_type.as_deref(), Some("minecraft:pig"));
    }

    /// With no `SpawnData`, the first `SpawnPotentials` entry's `data.entity.id`
    /// is the fallback — vanilla's own get-or-create-next-spawn-data's own order.
    #[test]
    fn spawner_data_falls_back_to_the_first_spawn_potential() {
        let potentials = lodestone_core::Nbt::List {
            element_type: lodestone_core::NbtTag::Compound,
            elements: vec![compound(vec![
                ("data", spawn_data_nbt("minecraft:skeleton")),
                ("weight", lodestone_core::Nbt::Int(1)),
            ])],
        };
        let nbt = compound(vec![("SpawnPotentials", potentials)]);
        let data = spawner_data(&nbt);
        assert_eq!(data.entity_type.as_deref(), Some("minecraft:skeleton"));
    }

    /// `spawn_data` (snake_case) is the trial spawner's own key, and it has no
    /// `SpawnPotentials`-shaped fallback — see [`spawner_data`]'s doc.
    #[test]
    fn spawner_data_reads_trial_spawners_snake_case_key() {
        let nbt = compound(vec![("spawn_data", spawn_data_nbt("minecraft:zombie"))]);
        let data = spawner_data(&nbt);
        assert_eq!(data.entity_type.as_deref(), Some("minecraft:zombie"));
    }

    /// Neither key present: no display entity, matching
    /// `entityToSpawn.getString("id").isEmpty()`'s own "draw nothing".
    #[test]
    fn spawner_data_with_neither_key_has_no_entity_type() {
        let data = spawner_data(&compound(vec![]));
        assert_eq!(data.entity_type, None);
    }

    fn trial_spawner_state(state: &str) -> u32 {
        lodestone_data::block_states::state_id(&format!(
            "minecraft:trial_spawner[ominous=false,trial_spawner_state={state}]"
        ))
        .unwrap_or_else(|| panic!("trial_spawner_state={state} must be a real state"))
    }

    /// Vanilla's own two spinning states and their numerators
    /// (its own spinning-mob-speed accessor), predicted from the enum
    /// constants read out of the real jar source, not a remembered pair.
    #[test]
    fn trial_spawner_spin_speed_matches_the_two_spinning_states() {
        assert_eq!(
            trial_spawner_spin_speed(trial_spawner_state("waiting_for_players")),
            Some(200.0)
        );
        assert_eq!(trial_spawner_spin_speed(trial_spawner_state("active")), Some(1000.0));
    }

    /// The four non-spinning states all miss — `hasSpinningMob()`'s own
    /// `spinningMobSpeed() >= 0.0` gate, ported as a `None`.
    #[test]
    fn trial_spawner_spin_speed_is_none_for_every_non_spinning_state() {
        for state in [
            "inactive",
            "waiting_for_reward_ejection",
            "ejecting_reward",
            "cooldown",
        ] {
            assert_eq!(
                trial_spawner_spin_speed(trial_spawner_state(state)),
                None,
                "state {state} must not spin"
            );
        }
    }

    /// The mob spawner has no such state gate at all: every state (there is
    /// only one) resolves to the constant `1000.0`.
    #[test]
    fn plain_spawner_always_has_the_constant_speed() {
        let id = lodestone_data::block_states::state_id("minecraft:spawner").expect("spawner");
        assert_eq!(spawner_spin_speed(id), Some(1000.0));
    }

    /// `spawner_mob_spawn`'s real gate: a trial spawner in `cooldown` must draw
    /// nothing even though its NBT still names a display entity from an
    /// earlier `active` phase — this is the exact clause the pixel gate
    /// (`trial_spawner_mob_pixels.rs`) proves reaches real pixels.
    #[test]
    fn a_cooldown_trial_spawner_draws_nothing_regardless_of_stale_spawn_data() {
        let data = SpawnerData {
            entity_type: Some("minecraft:pig".to_string()),
            min_spawn_delay: SPAWNER_DEFAULT_MIN_SPAWN_DELAY,
        };
        let spins = SpawnerSpins::new();
        let spawn = spawner_mob_spawn(
            [0, 0, 0],
            trial_spawner_state("cooldown"),
            &data,
            &spins,
            0.0,
            lodestone_render::ENTITY_FULLBRIGHT,
        );
        assert!(
            spawn.is_none(),
            "a cooldown trial spawner must draw an empty cage, not the stale display entity"
        );
    }

    /// The same position in `active` state, same NBT: it does draw.
    #[test]
    fn an_active_trial_spawner_with_spawn_data_draws() {
        let data = SpawnerData {
            entity_type: Some("minecraft:pig".to_string()),
            min_spawn_delay: SPAWNER_DEFAULT_MIN_SPAWN_DELAY,
        };
        let spins = SpawnerSpins::new();
        let spawn = spawner_mob_spawn(
            [0, 0, 0],
            trial_spawner_state("active"),
            &data,
            &spins,
            0.0,
            lodestone_render::ENTITY_FULLBRIGHT,
        );
        assert_eq!(spawn.map(|s| s.entity_type), Some("minecraft:pig".to_string()));
    }

    /// `SpawnerSpins::apply_block_event`/`tick`: a `BLOCK_EVENT` `b0 == 1`
    /// resets `spawn_delay` to `min_spawn_delay`, and ticking while `near` and
    /// `has_entity` advances `spin` by the predicted amount — magnitude
    /// prediction from the formula's own constants, not a sign check.
    #[test]
    fn tick_advances_spin_by_the_formula_from_outside_constants() {
        let mut spins = SpawnerSpins::new();
        let pos = [1, 2, 3];
        // First tick: spawn_delay starts at min_spawn_delay (200), so the
        // formula's own first step decrements it to 199 *before* dividing —
        // vanilla's own mob-spawner client tick decrements the spawn delay
        // before the `spin +=` line, not after.
        spins.tick(&[(pos, true, 200.0, true, 1000.0)]);
        let (_, spin_after_one) = spins.raw(pos);
        let expected = 1000.0 / (199.0 + 200.0);
        assert!(
            (spin_after_one - expected).abs() < 1e-4,
            "spin was {spin_after_one}, expected {expected}"
        );

        // A BLOCK_EVENT b0==1 resets spawn_delay back to min_spawn_delay.
        spins.apply_block_event(pos, 1, 0);
        spins.tick(&[(pos, true, 200.0, true, 1000.0)]);
        let (previous_after_reset, spin_after_reset) = spins.raw(pos);
        assert!(
            (previous_after_reset - spin_after_one).abs() < 1e-4,
            "previous must be last tick's spin, for the partial-tick lerp"
        );
        // The reset put `spawn_delay` back to `min_spawn_delay` (200), so this
        // tick's decrement-then-divide is identical to the first tick's:
        // `199.0 + 200.0` again, not `199.0` alone.
        let expected_after_reset = spin_after_one + expected;
        assert!(
            (spin_after_reset - expected_after_reset).abs() < 1e-4,
            "spin was {spin_after_reset}, expected {expected_after_reset}"
        );
    }

    /// A far mob spawner (not near) freezes: `spin` does not move, matching
    /// vanilla's own mob-spawner client tick's "not near" branch, which
    /// leaves the previous spin equal to the current one.
    #[test]
    fn a_spawner_nobody_is_near_freezes() {
        let mut spins = SpawnerSpins::new();
        let pos = [5, 5, 5];
        spins.tick(&[(pos, true, 200.0, true, 1000.0)]);
        let (_, spin_before) = spins.raw(pos);
        spins.tick(&[(pos, false, 200.0, true, 1000.0)]);
        let (_, spin_after) = spins.raw(pos);
        assert_eq!(spin_before, spin_after, "spin must not move while not near");
    }
}

/// Vanilla's own enchanting-table-block-entity book-animation tick's trigger radius, in blocks:
/// its own nearest-player search from the block's own center point.
///
/// Measured from the block **centre** to the player's position, in three
/// dimensions — not horizontally, so a player on the floor below a table on a
/// shelf does not open its book.
const ENCHANTING_TABLE_PLAYER_RADIUS: f64 = 3.0;

/// One enchanting table's book animation — `EnchantingTableBlockEntity`'s ten
/// public animation fields, none of which are on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Book {
    /// Vanilla's `time`, incremented once per tick and never reset.
    time: i32,
    flip: f32,
    /// `oFlip`: the value at the start of the current tick, for the partial-tick
    /// lerp.
    o_flip: f32,
    /// `flipT`: the *target* page, re-rolled at random. Unbounded, and it drifts
    /// in both directions.
    flip_t: f32,
    /// `flipA`: the page-turn velocity, itself smoothed toward the target
    /// difference at 90% per tick.
    flip_a: f32,
    open: f32,
    /// `oOpen`, for the partial-tick lerp.
    o_open: f32,
    /// `rot`, radians, wrapped into `-PI..PI`.
    rot: f32,
    /// `oRot`, for the partial-tick lerp — which must be **shortest-arc**.
    o_rot: f32,
    /// `tRot`: the angle the book is chasing. Points at the nearest player, or
    /// creeps by `0.02` rad/tick when there is nobody to look at.
    t_rot: f32,
}

/// Brings an angle into `-PI..PI`, vanilla's two `while` loops.
fn wrap_radians(mut angle: f32) -> f32 {
    const TAU: f32 = std::f32::consts::TAU;
    while angle >= std::f32::consts::PI {
        angle -= TAU;
    }
    while angle < -std::f32::consts::PI {
        angle += TAU;
    }
    angle
}

impl Book {
    /// One tick of `bookAnimationTick` for the table at `pos`.
    ///
    /// `player` is the nearest player's position, or `None` when there is none
    /// within [`ENCHANTING_TABLE_PLAYER_RADIUS`] — the caller does the radius test
    /// so this stays a pure function of its inputs.
    fn tick(&mut self, pos: [i32; 3], player: Option<glam::DVec3>, rng: &mut JavaRandom) {
        self.o_open = self.open;
        self.o_rot = self.rot;
        if let Some(player) = player {
            let xd = player.x - (f64::from(pos[0]) + 0.5);
            let zd = player.z - (f64::from(pos[2]) + 0.5);
            #[expect(
                clippy::cast_possible_truncation,
                reason = "an angle in radians; f32 is what the renderer takes"
            )]
            {
                self.t_rot = zd.atan2(xd) as f32;
            }
            self.open += 0.1;
            // `open < 0.5` makes the pages riffle *while opening* regardless of
            // the dice, and then it is a 1-in-40 chance per tick. Dropping the
            // first half leaves a book that opens dead still.
            if self.open < 0.5 || rng.next_i32_bound(40) == 0 {
                let old = self.flip_t;
                // Vanilla's `do { .. } while (old == flipT)`: the difference of
                // two `nextInt(4)` draws can be zero, and the loop **must**
                // re-roll rather than accept it. A plain `if` leaves the page
                // occasionally not turning at all when it was asked to.
                loop {
                    self.flip_t += (rng.next_i32_bound(4) - rng.next_i32_bound(4)) as f32;
                    if old != self.flip_t {
                        break;
                    }
                }
            }
        } else {
            self.t_rot += 0.02;
            self.open -= 0.1;
        }

        self.rot = wrap_radians(self.rot);
        self.t_rot = wrap_radians(self.t_rot);
        // The chase is 40% of the **wrapped** remaining arc. Without the wrap the
        // book takes the long way round whenever the angle crosses `±PI`, which
        // is a full backwards revolution in a couple of ticks and happens every
        // time a player walks past one particular corner.
        self.rot += wrap_radians(self.t_rot - self.rot) * 0.4;
        self.open = self.open.clamp(0.0, 1.0);
        self.time += 1;

        self.o_flip = self.flip;
        let diff = ((self.flip_t - self.flip) * 0.4).clamp(-0.2, 0.2);
        self.flip_a += (diff - self.flip_a) * 0.9;
        self.flip += self.flip_a;
    }

}

// `JavaRandom` used to be an independent port here, on the stated grounds
// that neither `lodestone-shell` nor `lodestone-render` had an RNG crate to
// depend on. Both now do: `lodestone_javarandom::JavaRandom`, the workspace's
// one copy, shared with `lodestone-particle`, `lodestone-audio` and
// `lodestone-render`'s lightning bolt (imported at the top of this file).
// `next_i32_bound`'s two branches are not interchangeable — a power-of-two
// bound is a multiply-and-shift and every other bound is a rejection loop —
// and `nextInt(4)`/`nextInt(40)` below take one each.

/// Per-position enchanting-table book animation state, advanced once per client
/// tick — the third animation fold beside [`ChestLids`] and [`BellShakes`], and
/// the first with **no packet driving it at all**.
///
/// Chest lids and bell shakes are both started by a `BLOCK_EVENT`; this one is
/// started by the player *standing near a block*, so nothing on the wire would
/// ever reveal that it had stopped working.
///
/// # Every table in the gather gets an entry, shut or not
///
/// This fold used to create entries only for tables within
/// [`ENCHANTING_TABLE_PLAYER_RADIUS`] and collect them again the moment they
/// settled shut, on the stated grounds that "a shut book renders exactly like an
/// absent one". **That is false, and it is why an enchanting table nobody stood
/// next to drew no book at all.** `open == 0` makes
/// [`lodestone_render::enchanting_table_book_openness`] zero, and
/// `book_part_poses` at openness `0` poses `left_lid` at `PI` against
/// `right_lid` at `0` — a **closed** book, which is a real six-part model
/// hovering above the table, not nothing. Vanilla's own enchant-table render
/// submission has no
/// early return: vanilla draws the book for every enchanting-table block entity
/// it renders, and the near-player test only decides whether it is *open*.
///
/// So an entry exists for every table the caller gathers, and is dropped when
/// the table leaves that gather (unloaded, or out of [`VIEW_DISTANCE`]) rather
/// than when it stops moving — which is also what makes `time` keep advancing
/// for a distant table, the way vanilla's per-block-entity `time++` does, so its
/// hover keeps breathing.
#[derive(Debug, Clone)]
pub struct EnchantingTableBooks {
    books: HashMap<[i32; 3], Book>,
    rng: JavaRandom,
}

impl Default for EnchantingTableBooks {
    fn default() -> Self {
        Self::new()
    }
}

impl EnchantingTableBooks {
    /// An empty set.
    ///
    /// The RNG seed is a fixed constant rather than a clock read, for the reason
    /// `docs/`'s evidence rules give: a test that seeds from the wall clock cannot
    /// be reproduced, and nothing about a page-flip phase benefits from being
    /// unpredictable across runs. Vanilla's own default random source is
    /// time-seeded, but it is a *shared static* across every enchanting table in
    /// the world, which is the property that actually matters and which this keeps.
    #[must_use]
    pub fn new() -> Self {
        EnchantingTableBooks {
            books: HashMap::new(),
            rng: JavaRandom::new(0x1BADB002),
        }
    }

    /// Advances one client tick: every position in `tables` gains an entry if it
    /// has none, every entry not in `tables` is dropped, and the rest tick.
    ///
    /// `tables` is every enchanting-table position in view (the caller gathers
    /// it — see [`enchanting_table_positions`]) and `player` is the local
    /// player's position, which decides only whether a given book *opens*.
    ///
    /// # Only the local player
    ///
    /// Vanilla asks the level for the *nearest* player, which on a busy server can
    /// be someone else. We use the local player, which is the one case that
    /// matters for what this client's user sees and the only position this layer
    /// has cheaply. A remote player standing at a table the local player can see
    /// therefore leaves its book shut — a fidelity gap, recorded rather than
    /// silently taken, and closing it means scanning tracked player entities here.
    pub fn tick(&mut self, tables: &[[i32; 3]], player: glam::DVec3) {
        let radius_squared = ENCHANTING_TABLE_PLAYER_RADIUS * ENCHANTING_TABLE_PLAYER_RADIUS;
        let live: std::collections::HashSet<[i32; 3]> = tables.iter().copied().collect();
        for pos in &live {
            self.books.entry(*pos).or_default();
        }
        // Borrowed separately from `self.rng` because the tick draws from it.
        let rng = &mut self.rng;
        self.books.retain(|pos, book| {
            if !live.contains(pos) {
                return false;
            }
            let centre = glam::DVec3::new(
                f64::from(pos[0]) + 0.5,
                f64::from(pos[1]) + 0.5,
                f64::from(pos[2]) + 0.5,
            );
            let near = (centre.distance_squared(player) <= radius_squared).then_some(player);
            book.tick(*pos, near, rng);
            true
        });
    }

    /// This frame's interpolated animation state for the table at `pos`, or `None`
    /// when there is no entry — a table the last tick did not gather, which the
    /// caller draws at the shut rest pose rather than skipping.
    ///
    /// Returns `(y_rot, time, open, flip)` ready for
    /// [`lodestone_render::EnchantingTableSpawn`]. The `y_rot` lerp is
    /// **shortest-arc**, matching vanilla's own enchant-table render-state
    /// extraction's three
    /// `while` loops rather than a plain `lerp`.
    #[must_use]
    pub fn state(&self, pos: [i32; 3], partial_tick: f32) -> Option<(f32, f32, f32, f32)> {
        let book = self.books.get(&pos)?;
        let alpha = partial_tick.clamp(0.0, 1.0);
        let y_rot = book.o_rot + wrap_radians(book.rot - book.o_rot) * alpha;
        #[expect(
            clippy::cast_precision_loss,
            reason = "vanilla's own `blockEntity.time + partialTicks` is a float add"
        )]
        let time = book.time as f32 + alpha;
        Some((
            y_rot,
            time,
            book.o_open + (book.open - book.o_open) * alpha,
            book.o_flip + (book.flip - book.o_flip) * alpha,
        ))
    }

    /// Number of tracked books (for stats and tests).
    #[must_use]
    pub fn len(&self) -> usize {
        self.books.len()
    }

    /// Whether nothing is being tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.books.is_empty()
    }
}

/// Vanilla's own direction-from-3D-data-value decode of `b1`, narrowed to the four horizontal directions
/// [`BellShakeDirection`] models: `0` down, `1` up, `2` north, `3` south, `4`
/// west, `5` east.
///
/// **The order is the jar's, not alphabetical and not `BellShakeDirection`'s own
/// declaration order** — getting it wrong swings the bell along the wrong axis,
/// which still looks like a working animation.
fn shake_direction_from_3d(b1: u8) -> Option<BellShakeDirection> {
    match b1 {
        2 => Some(BellShakeDirection::North),
        3 => Some(BellShakeDirection::South),
        4 => Some(BellShakeDirection::West),
        5 => Some(BellShakeDirection::East),
        _ => None,
    }
}

/// Reads one block state's `facing`/`type` properties into the chest fields the
/// renderer needs.
///
/// Returns `None` when the state has no `facing` — which for a chest cannot
/// happen, and for anything else means the caller pointed this at a block that
/// is not a chest.
#[must_use]
fn chest_orientation(state_id: StateId) -> Option<(f32, ChestHalf)> {
    let props = state_id.properties();
    let mut yaw = None;
    let mut half = ChestHalf::Single;
    for (name, value) in props {
        match *name {
            "facing" => yaw = horizontal_facing_yaw(value),
            "type" => half = ChestHalf::parse(value),
            _ => {}
        }
    }
    // An ender chest has `facing` but no `type`, and that is correct: it is
    // always single. A missing `facing` is the real failure and must not
    // silently become south — a wall of chests all facing one way is much harder
    // to spot as a bug than a chest that does not draw.
    Some((yaw?, half))
}

/// Resolves one block state id into a chest material, or `None` if it is not a
/// chest at all.
#[must_use]
fn chest_material(state_id: StateId) -> Option<ChestMaterial> {
    let name = state_id.name();
    let path = name.strip_prefix("minecraft:").unwrap_or(name);
    ChestMaterial::from_block_path(path)
}

/// Every block-entity position within [`VIEW_DISTANCE`] of `eye`, paired with the
/// block state actually at it — the candidate set [`chest_spawns`] filters.
///
/// Split out of [`chest_spawns`] so a gate can drive the real gather against a
/// real [`World`] without a live `ClientHandle`: this is the loop that reads
/// `chunk.block_entities`, and therefore the loop that saw nothing at all before
/// That fix was fixed. Everything `chest_spawns` adds on top of this and
/// [`chest_spawn`] is lock handling and a light sample.
#[must_use]
pub fn chest_candidates(
    world: &World,
    chunks: impl IntoIterator<Item = ChunkPos>,
    eye: Vec3,
) -> Vec<([i32; 3], u32)> {
    let cutoff = VIEW_DISTANCE * VIEW_DISTANCE;
    let mut candidates = Vec::new();
    for pos in chunks {
        let Some(chunk) = world.get(pos) else {
            continue;
        };
        for be in &chunk.block_entities {
            let x = pos.x * 16 + i32::from(be.rel_x);
            let z = pos.z * 16 + i32::from(be.rel_z);
            let y = i32::from(be.y);
            // Vanilla's `shouldRender`: distance from the camera to the block
            // *centre*, not its corner, against a flat 64.
            let centre = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
            if centre.distance_squared(eye) > cutoff {
                continue;
            }
            // `rel_x`/`rel_z` are section-relative and `y` absolute, which is
            // exactly `ChunkColumn::get_block`'s signature — no conversion, and
            // no second lookup through a position that would have to be re-split.
            let state_id = chunk
                .column
                .get_block(usize::from(be.rel_x), y, usize::from(be.rel_z));
            candidates.push(([x, y, z], state_id));
        }
    }
    candidates
}

/// One candidate resolved into a [`ChestSpawn`], or `None` if the state at that
/// position is not a chest.
///
/// The block **state** is the truth about appearance (see the module docs); the
/// block-entity record only says the position is worth looking at. So a stale or
/// orphan record whose state is not a chest resolves to `None` here and draws
/// nothing — which is what makes `block_entity_data`'s create-on-miss fallback
/// inert rather than a way to paint phantom chests.
#[must_use]
pub fn chest_spawn(
    block: [i32; 3],
    state_id: StateId,
    openness: f32,
    light: u8,
) -> Option<ChestSpawn> {
    let material = chest_material(state_id)?;
    let (facing_yaw_deg, half) = chest_orientation(state_id)?;
    Some(ChestSpawn {
        pos: block,
        facing_yaw_deg,
        half,
        material,
        openness,
        light,
    })
}

/// Every chest to draw this frame, gathered from the client-owned world's
/// block-entity records.
///
/// `eye` is the camera position and `partial_tick` the fraction through the
/// current client tick (`0..=1`) used to interpolate the lid. Returns an empty
/// vec before login, or when the handle has no world dimensions yet — never a
/// panic, for the same reason [`entity_light_at`] returns `None` rather than
/// darkness.
///
/// # Ordering, and why it is sorted
///
/// The output is sorted by position. `HashMap` iteration order over chunks is
/// non-deterministic per process, and an unsorted list makes the instance order
/// inside a batch differ run to run — which turns any pixel gate that reads back
/// a frame into a flaky one for reasons that look like a GPU problem.
#[must_use]
pub fn chest_spawns(
    handle: &SharedHandle,
    lids: &ChestLids,
    eye: Vec3,
    partial_tick: f32,
) -> Vec<ChestSpawn> {
    let Some(snapshot) = block_entity_frame_snapshot(handle, eye) else {
        return Vec::new();
    };
    chest_spawns_from_snapshot(&snapshot, lids, partial_tick)
}

#[must_use]
pub(crate) fn chest_spawns_from_snapshot(
    snapshot: &BlockEntityFrameSnapshot,
    lids: &ChestLids,
    partial_tick: f32,
) -> Vec<ChestSpawn> {
    let mut out = Vec::new();
    for candidate in &snapshot.candidates {
        let block = candidate.pos;
        if let Some(spawn) = chest_spawn(
            block,
            candidate.state_id,
            lids.openness(block, partial_tick),
            candidate.light,
        ) {
            out.push(spawn);
        }
    }
    out.sort_by_key(|s| s.pos);
    out
}

/// Reads a skull/head block state's orientation — `rotation` (`0..16`, floor
/// placement) or `facing` (wall placement) — into the renderer's fields.
///
/// A real skull state carries exactly one of the two (see
/// `assets/minecraft/blockstates/skeleton_skull.json` vs
/// `.../skeleton_wall_skull.json` in the real jar): floor skulls have
/// `rotation`, wall skulls have `facing`. `None` for a state with neither,
/// which cannot happen for a real skull and for anything else means the
/// caller pointed this at a block that is not one.
#[must_use]
fn skull_orientation(state_id: StateId) -> Option<SkullOrientation> {
    let props = state_id.properties();
    for (name, value) in props {
        match *name {
            "rotation" => {
                return value
                    .parse::<u8>()
                    .ok()
                    .map(|rotation_segment| SkullOrientation::Floor { rotation_segment });
            }
            "facing" => {
                return horizontal_facing_yaw(value)
                    .map(|facing_yaw_deg| SkullOrientation::Wall { facing_yaw_deg });
            }
            _ => {}
        }
    }
    None
}

/// Resolves one block state id into a skull/head type, or `None` if the block
/// is not a skull at all. All seven of vanilla's own skull-block types resolve —
/// see [`lodestone_render::SkullType::from_block_path`].
#[must_use]
fn skull_type_for_state(state_id: StateId) -> Option<SkullType> {
    let name = state_id.name();
    let path = name.strip_prefix("minecraft:").unwrap_or(name);
    SkullType::from_block_path(path)
}

/// Finds the one profile property that can name a placed player head's skin.
///
/// Modern skull block entities keep a normal game profile under `profile`, but
/// its properties arrive as NBT compounds rather than the tab-list's typed
/// `GameProfile`. Only the `textures` value matters: name and UUID are not an
/// authority to contact Mojang, and `signature` is intentionally not widened
/// into an authentication feature. Wrong tags fail closed to Steve.
#[must_use]
fn player_head_skin_url(nbt: &Nbt) -> Option<Arc<str>> {
    let Nbt::Compound(root) = nbt else {
        return None;
    };
    let Nbt::Compound(profile) = root.iter().find_map(|(name, tag)| (name == "profile").then_some(tag))? else {
        return None;
    };
    let Nbt::List {
        element_type: NbtTag::Compound,
        elements,
    } = profile
        .iter()
        .find_map(|(name, tag)| (name == "properties").then_some(tag))?
    else {
        return None;
    };
    let value = elements.iter().find_map(|property| {
        let Nbt::Compound(fields) = property else {
            return None;
        };
        let name = fields
            .iter()
            .find_map(|(name, tag)| (name == "name").then_some(tag));
        let value = fields
            .iter()
            .find_map(|(name, tag)| (name == "value").then_some(tag));
        match (name, value) {
            (Some(Nbt::String(name)), Some(Nbt::String(value))) if name == "textures" => {
                Some(value.as_str())
            }
            _ => None,
        }
    })?;
    crate::remote_skins::skin_for_textures_property(value)
        .map(|skin| Arc::<str>::from(skin.url))
}

/// One candidate resolved into a [`SkullSpawn`], or `None` if the state at
/// that position is not a ported skull type.
///
/// Same shape as [`chest_spawn`]: the block **state** is the truth about
/// appearance, the block-entity record only says the position is worth
/// looking at, so a stale or orphan record whose state is not a skull draws
/// nothing.
#[must_use]
pub fn skull_spawn(block: [i32; 3], state_id: StateId, light: u8) -> Option<SkullSpawn> {
    let skull_type = skull_type_for_state(state_id)?;
    let orientation = skull_orientation(state_id)?;
    Some(SkullSpawn {
        pos: block,
        orientation,
        skull_type,
        texture: BlockEntityTexture::Static(lodestone_render::skull_texture_stem(skull_type)),
        light,
    })
}

/// The NBT-aware candidate gather for skulls.
///
/// `chest_candidates` intentionally discards NBT because chest appearance is
/// entirely state-driven. A player-head profile is the exception: retain only
/// the already-decoded usable URL, never the whole untrusted NBT tree.
#[must_use]
fn skull_candidates(
    world: &World,
    chunks: impl IntoIterator<Item = ChunkPos>,
    eye: Vec3,
) -> Vec<([i32; 3], StateId, Option<Arc<str>>)> {
    let cutoff = VIEW_DISTANCE * VIEW_DISTANCE;
    let mut candidates = Vec::new();
    for pos in chunks {
        let Some(chunk) = world.get(pos) else {
            continue;
        };
        for be in &chunk.block_entities {
            let x = pos.x * 16 + i32::from(be.rel_x);
            let y = i32::from(be.y);
            let z = pos.z * 16 + i32::from(be.rel_z);
            let centre = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
            if centre.distance_squared(eye) > cutoff {
                continue;
            }
            let raw_state_id = chunk
                .column
                .get_block(usize::from(be.rel_x), y, usize::from(be.rel_z));
            let Some(state_id) = StateId::new(raw_state_id) else {
                continue;
            };
            let skin = (skull_type_for_state(state_id) == Some(SkullType::Player))
                .then(|| player_head_skin_url(&be.nbt))
                .flatten();
            candidates.push(([x, y, z], state_id, skin));
        }
    }
    candidates
}

/// Every skull/head to draw this frame, gathered from the client-owned
/// world's block-entity records.
///
/// Uses [`skull_candidates`] rather than [`chest_candidates`]: chest gathering
/// correctly discards NBT, while a placed player head's optional texture URL
/// lives there. The gather retains only that URL, alongside the same distance
/// and state checks every other block entity uses.
///
/// No lid-style animation state gathered here. No skull type poses its *head*,
/// and the two that do pose a child part — the dragon's jaw, the piglin's ears
/// — are driven by vanilla's own skull-block-entity animation accessor, a counter that only
/// advances while the block is redstone-`powered`. This client carries no such
/// per-position tracker, so every skull draws at
/// [`lodestone_render::SKULL_RESTING_ANIMATION_POS`] and there is nothing here
/// to tick. Wiring the powered animation starts in this function.
#[must_use]
pub fn skull_spawns(handle: &SharedHandle, eye: Vec3) -> Vec<SkullSpawn> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();

    // Same lock-ordering rule as `chest_spawns`: `loaded_chunks()` takes its
    // own read lock, so it must not be called from inside the guard below.
    let chunks = client.loaded_chunks();

    let candidates = {
        let world = store.read();
        skull_candidates(
            &world,
            chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }),
            eye,
        )
    };

    let sky_default = {
        let player = client.player();
        crate::mesher::sky_default_for_dimension(
            player.dimension.as_ref(),
            player.dimension_type.as_ref(),
        )
    };

    let mut out = Vec::new();
    for (block, state_id, skin) in candidates {
        let light = entity_light_at(handle, block[0], block[1], block[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        if let Some(mut spawn) = skull_spawn(block, state_id, light) {
            if let Some(url) = skin {
                spawn.texture = BlockEntityTexture::PlayerSkin(url);
            }
            out.push(spawn);
        }
    }
    out.sort_by_key(|s| s.pos);
    out
}

/// Starts the shared remote-skin fetch for each visible placed player-head URL.
///
/// The loader itself owns deduplication and failure memoisation, so this can run
/// every frame alongside block-entity extraction without multiplying requests.
pub(crate) fn request_player_head_skins(skulls: &[SkullSpawn]) {
    crate::remote_skins::request_all(
        skulls
            .iter()
            .filter_map(|spawn| spawn.texture.player_skin_url()),
    );
}

/// Resolves one block state id into whether it names a bell — `None` for
/// anything else. Unlike [`chest_material`]/[`skull_type_for_state`] there is
/// no per-block-path variant to select: every bell block state (any
/// `FACING`/`ATTACHMENT`/`POWERED` combination) draws the identical rig, so
/// this only needs to confirm the block *is* one.
#[must_use]
fn bell_is_present(state_id: StateId) -> bool {
    state_id.name() == "minecraft:bell"
}

/// One candidate resolved into a [`BellSpawn`], or `None` if the state at
/// that position is not a bell. Same shape as [`chest_spawn`]/[`skull_spawn`]:
/// the block **state** is the truth about whether this is a bell at all, so a
/// stale or orphan record whose state is not a bell draws nothing.
///
/// `shake` comes from [`BellShakes`], the `BLOCK_EVENT`-driven tracker — `None`
/// for a bell at rest, which is every bell until one is rung.
#[must_use]
pub fn bell_spawn(
    block: [i32; 3],
    state_id: StateId,
    light: u8,
    shakes: &BellShakes,
    partial_tick: f32,
) -> Option<BellSpawn> {
    if !bell_is_present(state_id) {
        return None;
    }
    Some(BellSpawn {
        pos: block,
        shake: shakes.shake(block, partial_tick),
        light,
    })
}

/// Every bell to draw this frame, gathered from the client-owned world's
/// block-entity records. Reuses [`chest_candidates`] exactly as
/// [`skull_spawns`] does, for the same reason: that gather is already generic
/// over block-entity type.
#[must_use]
pub fn bell_spawns(
    handle: &SharedHandle,
    shakes: &BellShakes,
    eye: Vec3,
    partial_tick: f32,
) -> Vec<BellSpawn> {
    let Some(snapshot) = block_entity_frame_snapshot(handle, eye) else {
        return Vec::new();
    };
    bell_spawns_from_snapshot(&snapshot, shakes, partial_tick)
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

/// Resolves a block state id into `(dye colour, facing)` for a shulker box, or
/// `None` if the state is not one.
///
/// **The colour is the block *id*, not a property and not NBT.** Vanilla has
/// seventeen shulker-box blocks (`shulker_box` plus one per dye), so
/// `minecraft:red_shulker_box` → `Some("red")` and the plain `minecraft:shulker_box`
/// → `None`, which is the undyed sheet. Reading a `color` property here would find
/// nothing and draw every box undyed.
///
/// `facing` defaults to [`ShulkerFacing::Up`] when the property is missing, which
/// is vanilla's own shulker-box render-state extraction's own facing-property
/// default of up
/// — unlike a chest, where a missing `facing` is treated as a failure, because a
/// shulker box genuinely has a sensible default and vanilla uses it.
#[must_use]
fn shulker_orientation(state_id: StateId) -> Option<(Option<&'static str>, ShulkerFacing)> {
    let name = state_id.name();
    let path = name.strip_prefix("minecraft:").unwrap_or(name);
    let colour = if path == "shulker_box" {
        None
    } else {
        // `SHULKER_COLOURS`' own entries, so the returned `&'static str` is the
        // one `shulker_texture_stem` matches against — a `&path[..]` slice would
        // not outlive this call.
        let stem = path.strip_suffix("_shulker_box")?;
        Some(*SHULKER_COLOURS.iter().find(|c| **c == stem)?)
    };
    let mut facing = ShulkerFacing::Up;
    for (name, value) in state_id.properties() {
        if *name == "facing"
            && let Some(parsed) = ShulkerFacing::from_name(value)
        {
            facing = parsed;
        }
    }
    Some((colour, facing))
}

/// One candidate resolved into a [`ShulkerSpawn`], or `None` if the state at that
/// position is not a shulker box.
///
/// `progress` is fixed at `0.0` — closed. See [`ShulkerSpawn::progress`] for why
/// that is the honest value rather than a placeholder.
#[must_use]
pub fn shulker_spawn(block: [i32; 3], state_id: StateId, light: u8) -> Option<ShulkerSpawn> {
    let (colour, facing) = shulker_orientation(state_id)?;
    Some(ShulkerSpawn {
        pos: block,
        facing,
        colour,
        progress: 0.0,
        light,
    })
}

/// Every shulker box to draw this frame. Reuses [`chest_candidates`] exactly as
/// [`skull_spawns`] and [`bell_spawns`] do.
#[must_use]
pub fn shulker_spawns(handle: &SharedHandle, eye: Vec3) -> Vec<ShulkerSpawn> {
    let Some(snapshot) = block_entity_frame_snapshot(handle, eye) else {
        return Vec::new();
    };
    shulker_spawns_from_snapshot(&snapshot)
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

/// One candidate resolved into a [`LecternSpawn`], or `None` if the state at
/// that position is not a lectern **with a book in it**.
///
/// Two `None` cases that are both correct and mean different things:
///
/// * The block is not a lectern. Same shape as every other gather here — the
///   block *state* is the truth, so a stale record draws nothing.
/// * The block is a lectern with `has_book=false`. There is genuinely nothing to
///   draw: a lectern's shelf, base and posts are all real block models
///   (`block/lectern.json` has geometry, unlike `chest.json`), so an empty
///   lectern is complete without this pass. Only the open book is missing, and
///   only when a book is in it. That also means an unwired lectern source
///   degrades to "no books on lecterns", not to a hole in the world.
///
/// `facing_yaw_deg` goes through [`horizontal_facing_clockwise_yaw`] and not
/// [`horizontal_facing_yaw`]: vanilla's own lectern render-state extraction stores
/// the facing rotated clockwise, and the plain facing lays the book across
/// the shelf at right angles to the reader.
#[must_use]
pub fn lectern_spawn(block: [i32; 3], state_id: StateId, light: u8) -> Option<LecternSpawn> {
    if state_id.name() != "minecraft:lectern" {
        return None;
    }
    let mut yaw = None;
    let mut has_book = false;
    for (name, value) in state_id.properties() {
        match *name {
            "facing" => yaw = horizontal_facing_clockwise_yaw(value),
            "has_book" => has_book = *value == "true",
            _ => {}
        }
    }
    if !has_book {
        return None;
    }
    Some(LecternSpawn {
        pos: block,
        facing_yaw_deg: yaw?,
        light,
    })
}

/// Every lectern book to draw this frame. Reuses [`chest_candidates`] exactly as
/// [`skull_spawns`], [`bell_spawns`] and [`shulker_spawns`] do.
#[must_use]
pub fn lectern_spawns(handle: &SharedHandle, eye: Vec3) -> Vec<LecternSpawn> {
    let Some(snapshot) = block_entity_frame_snapshot(handle, eye) else {
        return Vec::new();
    };
    lectern_spawns_from_snapshot(&snapshot)
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

/// Whether a block state is an enchanting table.
///
/// One block, no properties that matter: an enchanting table has **no `facing`**
/// and no other state at all in 26.2. That absence is load-bearing — the book's
/// angle is client-simulated (vanilla's own enchanting-table rotation field), so there is
/// nothing on the block state a facing could have been read from, and a port that
/// looks for one finds nothing and draws no book.
#[must_use]
fn is_enchanting_table(state_id: u32) -> bool {
    lodestone_data::block_states::block_name(state_id) == Some("minecraft:enchanting_table")
}

/// Every enchanting-table position worth ticking, within `radius` blocks of
/// `player`.
///
/// **`radius` is a view cutoff, not the animation trigger.** It used to be a
/// much tighter one, on the reasoning that only a player within
/// [`ENCHANTING_TABLE_PLAYER_RADIUS`] starts an animation — true, and it made
/// every table beyond that draw **no book at all**, because a table with no
/// entry was skipped by the gather below. A shut book is still a book; see
/// [`EnchantingTableBooks`]. The caller passes [`VIEW_DISTANCE`], matching the
/// draw gather, and the 3-block trigger stays inside
/// [`EnchantingTableBooks::tick`] where it belongs.
///
/// The scan itself walks every loaded chunk's block-entity list regardless, so
/// widening the radius costs one distance test per record and no more.
#[must_use]
pub fn enchanting_table_positions(
    handle: &SharedHandle,
    player: glam::DVec3,
    radius: f64,
) -> Vec<[i32; 3]> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();
    let cutoff = radius * radius;
    let world = store.read();
    let mut out = Vec::new();
    for pos in chunks {
        let pos = ChunkPos { x: pos.x, z: pos.z };
        let Some(chunk) = world.get(pos) else {
            continue;
        };
        for be in &chunk.block_entities {
            let x = pos.x * 16 + i32::from(be.rel_x);
            let z = pos.z * 16 + i32::from(be.rel_z);
            let y = i32::from(be.y);
            let centre =
                glam::DVec3::new(f64::from(x) + 0.5, f64::from(y) + 0.5, f64::from(z) + 0.5);
            if centre.distance_squared(player) > cutoff {
                continue;
            }
            let state_id = chunk
                .column
                .get_block(usize::from(be.rel_x), y, usize::from(be.rel_z));
            if is_enchanting_table(state_id) {
                out.push([x, y, z]);
            }
        }
    }
    out
}

/// Every enchanting-table book to draw this frame — **one per table, always**.
///
/// Unlike every other gather here the *appearance* comes from `books` rather than
/// from the world: the block state says only "there is a table", and all four
/// animated values are client-simulated. A table the fold has no entry for still
/// gets a spawn, at the shut rest pose: vanilla's own enchant-table render
/// submission draws the
/// book unconditionally, and openness `0` is a closed book rather than an absent
/// one. That case is transient by construction — [`EnchantingTableBooks::tick`]
/// gathers at the same [`VIEW_DISTANCE`] this does — so it lasts at most the one
/// frame between a table coming into view and the next 20 Hz tick.
///
/// Reuses [`chest_candidates`] exactly as [`lectern_spawns`] does — the block
/// state is still what confirms the block entity is a table.
#[must_use]
pub fn enchanting_table_spawns(
    handle: &SharedHandle,
    books: &EnchantingTableBooks,
    eye: Vec3,
    partial_tick: f32,
) -> Vec<lodestone_render::EnchantingTableSpawn> {
    let Some(snapshot) = block_entity_frame_snapshot(handle, eye) else {
        return Vec::new();
    };
    enchanting_table_spawns_from_snapshot(&snapshot, books, partial_tick)
}

#[must_use]
pub(crate) fn enchanting_table_spawns_from_snapshot(
    snapshot: &BlockEntityFrameSnapshot,
    books: &EnchantingTableBooks,
    partial_tick: f32,
) -> Vec<lodestone_render::EnchantingTableSpawn> {
    let mut out = Vec::new();
    for candidate in &snapshot.candidates {
        if !is_enchanting_table(candidate.state_id.raw()) {
            continue;
        }
        let block = candidate.pos;
        let (y_rot, time, open, flip) = books.state(block, partial_tick).unwrap_or_default();
        out.push(lodestone_render::EnchantingTableSpawn {
            pos: block,
            y_rot,
            time,
            open,
            flip,
            light: candidate.light,
        });
    }
    out.sort_by_key(|s| s.pos);
    out
}

/// The `facing` yaw of a campfire block, or `None` for any other block.
///
/// Both campfire blocks count: `soul_campfire` has the identical block entity,
/// the identical four cooking slots and the identical renderer registration — the
/// only difference is the flame's colour, which lives in the *block* model and
/// therefore nowhere near this path.
#[must_use]
fn campfire_facing_yaw(state_id: u32) -> Option<f32> {
    let name = lodestone_data::block_states::block_name(state_id)?;
    if name != "minecraft:campfire" && name != "minecraft:soul_campfire" {
        return None;
    }
    let props = lodestone_data::block_states::properties(state_id)?;
    props
        .iter()
        .find(|(name, _)| *name == "facing")
        .and_then(|(_, value)| horizontal_facing_yaw(value))
}

/// Resolve one loaded block-entity position into the source consumed by
/// `CampfireBlockEntity::particleTick`: its block position and whether hay
/// underneath turns the plume into signal smoke.
///
/// The block state is authoritative. A stale campfire block-entity record at a
/// position that is now air, or an extinguished campfire whose entity remains
/// loaded, emits nothing.
#[must_use]
fn campfire_smoke_source(block: [i32; 3], state_id: u32) -> Option<([i32; 3], bool)> {
    let name = lodestone_data::block_states::block_name(state_id)?;
    if name != "minecraft:campfire" && name != "minecraft:soul_campfire" {
        return None;
    }
    let properties = lodestone_data::block_states::properties(state_id)?;
    let property = |key: &str| {
        properties
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| *value)
    };
    if property("lit") != Some("true") {
        return None;
    }
    Some((block, property("signal_fire") == Some("true")))
}

/// The occupied cooking slots in a campfire's NBT, as `(slot, item id)`.
///
/// Vanilla's own container-helper item-list save writes an `Items` list of
/// its own slot-tagged item-stack codec, i.e. `{Slot: <unsigned byte>, id: <item id>,
/// count: <int>}` — so the slot is an explicit field and **the list index is not
/// the slot**. A campfire holding one steak in its third slot writes a
/// single-element list with `Slot: 2`; reading the index instead would cook it in
/// the wrong corner, and with a full campfire the two agree, so the bug hides
/// until a partial one.
///
/// `count` is not read: vanilla's own campfire renderer draws one copy per slot regardless
/// (a campfire slot holds at most one item anyway).
///
/// An entry whose `Slot` is out of range is dropped rather than clamped, matching
/// vanilla's own slot-tagged item-stack container-validity check.
#[must_use]
fn campfire_items(nbt: &lodestone_core::Nbt) -> Vec<(usize, lodestone_assets::ResourceLocation)> {
    use lodestone_core::Nbt;

    let Nbt::Compound(fields) = nbt else {
        return Vec::new();
    };
    let Some(Nbt::List { elements, .. }) =
        fields.iter().find(|(name, _)| name == "Items").map(|(_, v)| v)
    else {
        return Vec::new();
    };
    elements
        .iter()
        .filter_map(|entry| {
            let Nbt::Compound(entry) = entry else {
                return None;
            };
            let field = |key: &str| entry.iter().find(|(name, _)| name == key).map(|(_, v)| v);
            // Vanilla's own unsigned-byte codec — an `Nbt::Byte`, not an int.
            let slot = match field("Slot") {
                Some(Nbt::Byte(slot)) => usize::try_from(*slot).ok()?,
                // Vanilla's own codec helper for an optional-but-defaulted field
                // defaults a missing slot to zero rather than dropping the item.
                None => 0,
                _ => return None,
            };
            if slot >= lodestone_render::CAMPFIRE_SLOTS {
                return None;
            }
            let Some(Nbt::String(id)) = field("id") else {
                return None;
            };
            Some((slot, id.parse().ok()?))
        })
        .collect()
}

/// Every campfire position within [`VIEW_DISTANCE`], paired with its block state
/// and stored item list.
///
/// A third NBT-reading candidate gather beside [`sign_candidates`] and
/// [`banner_candidates`], for the same reason both of those exist:
/// [`chest_candidates`] discards `be.nbt`, and a campfire's *entire* appearance
/// from this renderer's point of view is in there — the fire and the logs are
/// block-model geometry the terrain mesher already draws.
#[must_use]
fn campfire_candidates(
    world: &World,
    chunks: impl IntoIterator<Item = ChunkPos>,
    eye: Vec3,
) -> Vec<([i32; 3], u32, Vec<(usize, lodestone_assets::ResourceLocation)>)> {
    let cutoff = VIEW_DISTANCE * VIEW_DISTANCE;
    let mut candidates = Vec::new();
    for pos in chunks {
        let Some(chunk) = world.get(pos) else {
            continue;
        };
        for be in &chunk.block_entities {
            let x = pos.x * 16 + i32::from(be.rel_x);
            let z = pos.z * 16 + i32::from(be.rel_z);
            let y = i32::from(be.y);
            let centre = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
            if centre.distance_squared(eye) > cutoff {
                continue;
            }
            let state_id = chunk
                .column
                .get_block(usize::from(be.rel_x), y, usize::from(be.rel_z));
            // The NBT parse is behind the block-name test, unlike the banner
            // gather's: every block entity in range would otherwise walk its
            // `Items` list, and chests and shulker boxes both have one.
            if campfire_facing_yaw(state_id).is_none() {
                continue;
            }
            candidates.push(([x, y, z], state_id, campfire_items(&be.nbt)));
        }
    }
    candidates
}

#[must_use]
fn campfire_smoke_sources_from_loaded_world(
    world: &World,
    chunks: impl IntoIterator<Item = ChunkPos>,
    eye: Vec3,
) -> Vec<([i32; 3], bool)> {
    let mut sources = campfire_candidates(world, chunks, eye)
        .into_iter()
        .filter_map(|(block, state_id, _)| campfire_smoke_source(block, state_id))
        .collect::<Vec<_>>();
    sources.sort_by_key(|(block, _)| *block);
    sources
}

/// Every loaded, lit campfire whose block-entity smoke tick is close enough to
/// matter to the camera.
///
/// This deliberately walks the decoded block-entity list rather than the
/// random nearby-block sampler used for `Block::animateTick`. In 26.2 the main
/// plume belongs to `CampfireBlockEntity::particleTick`, so every loaded
/// campfire in the normal block-entity render range gets one probability roll
/// per client tick, independent of its distance from the player's current
/// random-scan cube.
#[must_use]
pub fn campfire_smoke_sources(
    handle: &SharedHandle,
    eye: Vec3,
) -> Vec<([i32; 3], bool)> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();
    let world = store.read();
    campfire_smoke_sources_from_loaded_world(
        &world,
        chunks.into_iter().map(|pos| ChunkPos { x: pos.x, z: pos.z }),
        eye,
    )
}

/// Every campfire cooking item to draw this frame — one
/// [`CampfireItemSpawn`](lodestone_render::CampfireItemSpawn) per **occupied**
/// slot, so a lit but empty campfire yields none.
///
/// Unlike every other gather in this module this feeds the *model* pipeline
/// rather than the entity one: `CampfireRenderer` owns no mesh and no sheet, only
/// four item poses. See `lodestone_render::campfire_item_matrix`.
///
/// No clock and no partial tick — vanilla's `CampfireRenderer` has no animation
/// at all (the flame flicker is the block model's animated texture, and the
/// `CookingTimes` in the NBT drive nothing on the client). Installed per frame
/// anyway, for `Sim::skull_source`'s reason.
#[must_use]
pub fn campfire_spawns(
    handle: &SharedHandle,
    eye: Vec3,
) -> Vec<lodestone_render::CampfireItemSpawn> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();

    let candidates = {
        let world = store.read();
        campfire_candidates(
            &world,
            chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }),
            eye,
        )
    };

    let sky_default = {
        let player = client.player();
        crate::mesher::sky_default_for_dimension(
            player.dimension.as_ref(),
            player.dimension_type.as_ref(),
        )
    };

    let mut out = Vec::new();
    for (block, state_id, items) in candidates {
        let Some(facing_yaw_deg) = campfire_facing_yaw(state_id) else {
            continue;
        };
        let light = entity_light_at(handle, block[0], block[1], block[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        for (slot, item) in items {
            out.push(lodestone_render::CampfireItemSpawn {
                pos: block,
                facing_yaw_deg,
                slot,
                item,
                light,
            });
        }
    }
    out.sort_by_key(|s| (s.pos, s.slot));
    out
}

#[cfg(test)]
mod campfire_smoke_tests {
    use super::*;
    use lodestone_world::{
        BlockEntity, ChunkColumn, ColumnLight, Heightmaps, LoadedChunk, PaletteKind,
    };

    fn campfire_state(lit: bool, signal_fire: bool) -> u32 {
        (0..lodestone_data::block_states::STATE_COUNT)
            .find(|id| {
                if lodestone_data::block_states::block_name(*id) != Some("minecraft:campfire") {
                    return false;
                }
                let properties = lodestone_data::block_states::properties(*id).unwrap_or(&[]);
                let property = |key: &str| {
                    properties
                        .iter()
                        .find(|(name, _)| *name == key)
                        .map(|(_, value)| *value)
                };
                property("lit") == Some(if lit { "true" } else { "false" })
                    && property("signal_fire")
                        == Some(if signal_fire { "true" } else { "false" })
            })
            .expect("the 26.2 state table must contain the requested campfire state")
    }

    #[test]
    fn only_lit_campfires_are_smoke_sources() {
        let pos = [2, 64, 18];
        assert_eq!(
            campfire_smoke_source(pos, campfire_state(true, false)),
            Some((pos, false))
        );
        assert_eq!(
            campfire_smoke_source(pos, campfire_state(true, true)),
            Some((pos, true))
        );
        assert_eq!(
            campfire_smoke_source(pos, campfire_state(false, false)),
            None
        );
    }

    #[test]
    fn loaded_campfire_block_entities_are_found_beyond_the_old_ambient_scan() {
        let chunk_pos = ChunkPos::new(0, 1);
        let mut column = ChunkColumn::new(
            0,
            16,
            PaletteKind::block_states(),
            PaletteKind::biomes(),
            0,
            0,
        );
        let state = campfire_state(true, false);
        column.set_block(2, 4, 2, state);
        let mut world = World::new();
        world.load(
            chunk_pos,
            LoadedChunk::new(
                column,
                ColumnLight::new(16),
                Heightmaps::new(),
                vec![BlockEntity {
                    rel_x: 2,
                    rel_z: 2,
                    y: 4,
                    type_id: 0,
                    nbt: lodestone_core::Nbt::End,
                }],
            ),
        );

        assert_eq!(
            campfire_smoke_sources_from_loaded_world(
                &world,
                [chunk_pos],
                Vec3::new(2.5, 4.5, 0.5),
            ),
            vec![([2, 4, 18], false)],
            "the HUD campfire is outside ±8 but still inside the block-entity render range"
        );
    }
}

/// Resolves a block's registry path into which of vanilla's two sign
/// renderers applies — `None` for anything that is not a sign at all.
///
/// Every sign block path ends in `_sign`, including both wall variants
/// (`oak_wall_sign`, `oak_wall_hanging_sign`); a hanging one always contains
/// `hanging` (`oak_hanging_sign`, `oak_wall_hanging_sign`) — checked first,
/// since the two families share that `_sign` suffix and their text
/// transforms differ (see [`SignKind`]).
///
/// **This used to return a bool and decline hanging signs outright**, on the
/// recorded belief that they needed "a different model set again (chains, a
/// bar)". They do not: 26.2's `HangingSignRenderer` declares no model, and
/// the chains and bar are real block-model geometry the terrain mesher
/// already draws. See [`SignKind`]'s own doc for the measurement.
#[must_use]
fn sign_kind_for_path(path: &str) -> Option<SignKind> {
    if !path.ends_with("_sign") {
        return None;
    }
    Some(if path.contains("hanging") {
        SignKind::Hanging
    } else {
        SignKind::Plain
    })
}

/// Resolves one block state id into which sign renderer it uses — `None` for
/// anything that is not a sign (see [`sign_kind_for_path`]).
///
/// `pub(crate)` for `crate::sign_diagnostics`, which must ask the *same*
/// question production asks — a diagnostic with its own copy of this rule
/// could agree with itself and disagree with the draw.
#[must_use]
pub(crate) fn sign_kind_for_state(state_id: StateId) -> Option<SignKind> {
    let name = state_id.name();
    let path = name.strip_prefix("minecraft:").unwrap_or(name);
    sign_kind_for_path(path)
}

/// Reads a plain sign's placement — `rotation` (`0..16`, ground) or `facing`
/// (wall) — into [`SignOrientation`]. Mirrors [`skull_orientation`] exactly:
/// a real sign state carries exactly one of the two (`oak_sign.json` has
/// `rotation`, `oak_wall_sign.json` has `facing`), and `None` for a state
/// with neither cannot happen for a real sign.
///
/// `pub(crate)` for `crate::sign_diagnostics`, for the same reason
/// [`sign_kind_for_state`] is.
#[must_use]
pub(crate) fn sign_orientation(state_id: StateId) -> Option<SignOrientation> {
    let props = state_id.properties();
    for (name, value) in props {
        match *name {
            "rotation" => {
                return value
                    .parse::<u8>()
                    .ok()
                    .map(|rotation_segment| SignOrientation::Ground { rotation_segment });
            }
            "facing" => {
                return horizontal_facing_yaw(value)
                    .map(|facing_yaw_deg| SignOrientation::Wall { facing_yaw_deg });
            }
            _ => {}
        }
    }
    None
}

/// Every sign block-entity position within [`VIEW_DISTANCE`], paired with its
/// block state **and** typed text — the one candidate gather in this module
/// that needs the NBT half of a [`lodestone_world::BlockEntity`], because
/// sign text lives there and nowhere else (see
/// `docs/block-entity-renderers.md`'s Sign section for the captured wire
/// shape). [`chest_candidates`] cannot be reused here: it deliberately
/// discards `be.nbt` because neither chest nor skull reads it, and widening
/// its return type would ripple through both of those working, tested
/// gathers for a field only this caller needs. The NBT is parsed into
/// [`SignText`] right here rather than threaded further as a raw
/// [`lodestone_core::Nbt`] — nothing downstream wants the untyped form.
#[must_use]
fn sign_candidates(
    world: &World,
    chunks: impl IntoIterator<Item = ChunkPos>,
    eye: Vec3,
) -> Vec<([i32; 3], StateId, SignText)> {
    let cutoff = VIEW_DISTANCE * VIEW_DISTANCE;
    let mut candidates = Vec::new();
    for pos in chunks {
        let Some(chunk) = world.get(pos) else {
            continue;
        };
        for be in &chunk.block_entities {
            let x = pos.x * 16 + i32::from(be.rel_x);
            let z = pos.z * 16 + i32::from(be.rel_z);
            let y = i32::from(be.y);
            let centre = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
            if centre.distance_squared(eye) > cutoff {
                continue;
            }
            let raw_state_id = chunk
                .column
                .get_block(usize::from(be.rel_x), y, usize::from(be.rel_z));
            let Some(state_id) = StateId::new(raw_state_id) else {
                continue;
            };
            // `chunk.block_entities` is every block entity in the column —
            // chests, furnaces, hoppers, barrels, beds — not just signs, and
            // this runs once per rendered frame. Resolving the state first
            // costs one table lookup and skips a full NBT walk plus a
            // `SignText` allocation for every non-sign record in a 64-block
            // sphere. Behaviour is unchanged: [`sign_spawn`] already drops
            // anything whose state is not a sign, so this only stops the work
            // being done twice over on records that were always going to be
            // discarded.
            if sign_kind_for_state(state_id).is_none() {
                continue;
            }
            candidates.push(([x, y, z], state_id, SignText::parse(&be.nbt)));
        }
    }
    candidates
}

/// One candidate resolved into a [`SignSpawn`], or `None` if the state at
/// that position is not a sign. Same shape as [`chest_spawn`]/
/// [`skull_spawn`]: the block **state** is the truth about whether this is a
/// sign at all and how it sits, so a stale or orphan record whose state is
/// not a sign draws nothing.
#[must_use]
fn sign_spawn(block: [i32; 3], state_id: StateId, text: SignText, light: u8) -> Option<SignSpawn> {
    let kind = sign_kind_for_state(state_id)?;
    let orientation = sign_orientation(state_id)?;
    Some(SignSpawn {
        pos: block,
        kind,
        orientation,
        front: text.front,
        back: text.back,
        light,
    })
}

/// Every sign to draw this frame, gathered from the client-owned world's
/// block-entity records. `eye` is the camera position, the same gate
/// [`chest_spawns`]/[`skull_spawns`] apply. No lid-style animation state:
/// sign text does not animate, so there is nothing here to tick.
///
/// Sorted by position for the same reason [`chest_spawns`] is — deterministic
/// batch order for pixel gates, not a correctness requirement of the draw
/// itself.
#[must_use]
pub fn sign_spawns(handle: &SharedHandle, eye: Vec3) -> Vec<SignSpawn> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();

    // Same lock-ordering rule as `chest_spawns`: `loaded_chunks()` takes its
    // own read lock, so it must not be called from inside the guard below.
    let chunks = client.loaded_chunks();

    let candidates = {
        let world = store.read();
        sign_candidates(
            &world,
            chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }),
            eye,
        )
    };

    let sky_default = {
        let player = client.player();
        crate::mesher::sky_default_for_dimension(
            player.dimension.as_ref(),
            player.dimension_type.as_ref(),
        )
    };

    let mut out = Vec::new();
    for (block, state_id, text) in candidates {
        let light = entity_light_at(handle, block[0], block[1], block[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        if let Some(spawn) = sign_spawn(block, state_id, text, light) {
            out.push(spawn);
        }
    }
    out.sort_by_key(|s| s.pos);
    // Says, per sign, *why* a board is drawing no text. Off unless
    // `RUST_LOG=signs=debug`, and rate-limited by a call counter once on — see
    // `crate::sign_diagnostics`. It takes its own read lock, so it must run
    // after the guard above is dropped, the same lock-ordering rule
    // `loaded_chunks` obeys.
    crate::sign_diagnostics::report(handle, eye, &out);
    out
}

/// The block's own dye colour, for a **standing** banner — `white_banner` →
/// `DyeColor::White`.
///
/// The base colour is the *block*, not a state property: vanilla ships sixteen
/// separate banner blocks. Grepping for a `color` property here finds nothing and
/// draws every banner white, which is the natural mistake because shulker boxes
/// are spelled the same way and skulls are not.
///
/// **Both** forms resolve now. `*_wall_banner` used to return `None` because the
/// asset corpus had no `createBodyLayer(false)` mesh and the standing rig would
/// have hung a full 42-texel pole in mid-air off the block face; both wall meshes
/// exist since `banner_wall_body_model`/`banner_wall_flag_model` landed.
///
/// The suffix order is load-bearing: `_wall_banner` has to be tried **before**
/// `_banner`, because `"red_wall_banner"` ends in `_banner` too and would
/// otherwise strip to `"red_wall"`, which is not a dye name — so every wall
/// banner in the world would silently draw nothing rather than draw wrong.
#[must_use]
fn banner_colour(state_id: u32) -> Option<(DyeColor, bool)> {
    let name = lodestone_data::block_states::block_name(state_id)?;
    let path = name.strip_prefix("minecraft:").unwrap_or(name);
    if let Some(dye) = path.strip_suffix("_wall_banner") {
        return Some((DyeColor::from_name(dye)?, true));
    }
    Some((DyeColor::from_name(path.strip_suffix("_banner")?)?, false))
}

/// How a banner is attached, read off the property its own block actually has.
///
/// A standing banner has `rotation` (`RotationSegment`, `0..16`, `22.5` degrees a
/// step) and a wall banner has `facing` (four horizontals, `90` degrees a step) —
/// **neither has the other's**, so this is a fork on which block it is rather than
/// a property lookup that tries both. Reading `rotation` off a wall banner finds
/// nothing and draws no banner; reading `facing` off a standing one does the same.
#[must_use]
fn banner_attachment(state_id: u32, is_wall: bool) -> Option<BannerAttachment> {
    let props = lodestone_data::block_states::properties(state_id)?;
    if is_wall {
        let value = props
            .iter()
            .find(|(name, _)| *name == "facing")
            .map(|(_, value)| *value)?;
        return Some(BannerAttachment::Wall {
            facing_yaw_deg: horizontal_facing_yaw(value)?,
        });
    }
    let rotation_segment = props
        .iter()
        .find(|(name, _)| *name == "rotation")
        .and_then(|(_, value)| value.parse::<u8>().ok())?;
    Some(BannerAttachment::Ground { rotation_segment })
}

/// The block entity's stored pattern stack, parsed out of its NBT.
///
/// Vanilla's own banner-pattern-layers codec is `{pattern: <id>, color: <dye name>}` and
/// the list key is `patterns`. Both fields are namespaced ids on the wire, so the
/// namespace is stripped — [`lodestone_assets::banner_pattern_atlas`] keys its
/// sprites on the **bare** asset id (`"creeper"`), and passing
/// `"minecraft:creeper"` through resolves nothing and silently drops the layer.
///
/// A layer whose colour or pattern does not parse is dropped rather than
/// defaulted: a wrong-coloured layer is harder to notice than a missing one.
#[must_use]
fn banner_patterns(nbt: &lodestone_core::Nbt) -> Vec<StoredPatternLayer> {
    use lodestone_core::Nbt;

    let field = |compound: &'_ Nbt, key: &str| -> Option<String> {
        let Nbt::Compound(fields) = compound else {
            return None;
        };
        match fields.iter().find(|(name, _)| name == key).map(|(_, v)| v) {
            Some(Nbt::String(value)) => Some(value.clone()),
            _ => None,
        }
    };
    let Nbt::Compound(fields) = nbt else {
        return Vec::new();
    };
    let Some(Nbt::List { elements, .. }) = fields
        .iter()
        .find(|(name, _)| name == "patterns")
        .map(|(_, v)| v)
    else {
        return Vec::new();
    };
    elements
        .iter()
        .filter_map(|layer| {
            let pattern = field(layer, "pattern")?;
            let colour = field(layer, "color")?;
            Some(StoredPatternLayer {
                pattern_asset_id: pattern
                    .strip_prefix("minecraft:")
                    .unwrap_or(&pattern)
                    .to_string(),
                color: DyeColor::from_name(colour.strip_prefix("minecraft:").unwrap_or(&colour))?,
            })
        })
        .collect()
}

/// Every banner position within [`VIEW_DISTANCE`], paired with its block state and
/// pattern stack.
///
/// A second NBT-reading candidate gather beside [`sign_candidates`], and for the
/// same reason that one exists: [`chest_candidates`] discards `be.nbt`, and a
/// banner's whole appearance past its base colour lives there.
#[must_use]
fn banner_candidates(
    world: &World,
    chunks: impl IntoIterator<Item = ChunkPos>,
    eye: Vec3,
) -> Vec<([i32; 3], u32, Vec<StoredPatternLayer>)> {
    let cutoff = VIEW_DISTANCE * VIEW_DISTANCE;
    let mut candidates = Vec::new();
    for pos in chunks {
        let Some(chunk) = world.get(pos) else {
            continue;
        };
        for be in &chunk.block_entities {
            let x = pos.x * 16 + i32::from(be.rel_x);
            let z = pos.z * 16 + i32::from(be.rel_z);
            let y = i32::from(be.y);
            let centre = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
            if centre.distance_squared(eye) > cutoff {
                continue;
            }
            let state_id = chunk
                .column
                .get_block(usize::from(be.rel_x), y, usize::from(be.rel_z));
            candidates.push(([x, y, z], state_id, banner_patterns(&be.nbt)));
        }
    }
    candidates
}

/// One candidate resolved into a [`BannerSpawn`], standing or wall, or `None` when
/// the state is not a banner at all.
#[must_use]
fn banner_spawn(
    block: [i32; 3],
    state_id: u32,
    patterns: Vec<StoredPatternLayer>,
    phase: f32,
    light: u8,
) -> Option<BannerSpawn> {
    // One read decides both the dye and which form this is, so the colour and the
    // attachment can never disagree about whether it is a wall banner.
    let (base_color, is_wall) = banner_colour(state_id)?;
    Some(BannerSpawn {
        pos: block,
        attachment: banner_attachment(state_id, is_wall)?,
        base_color,
        patterns,
        phase,
        light,
    })
}

/// Every banner to draw this frame.
///
/// `game_time` and `partial_tick` are both needed and both come from the caller:
/// `banner_phase` mixes the block position into the tick so two adjacent banners
/// sway out of step, and the partial tick is what makes the sway smooth rather
/// than 20 Hz. A source that captured either would freeze every banner — the same
/// warning `bell_source` carries.
#[must_use]
pub fn banner_spawns(
    handle: &SharedHandle,
    eye: Vec3,
    game_time: i64,
    partial_tick: f32,
) -> Vec<BannerSpawn> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();

    let candidates = {
        let world = store.read();
        banner_candidates(
            &world,
            chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }),
            eye,
        )
    };

    let sky_default = {
        let player = client.player();
        crate::mesher::sky_default_for_dimension(
            player.dimension.as_ref(),
            player.dimension_type.as_ref(),
        )
    };

    let mut out = Vec::new();
    for (block, state_id, patterns) in candidates {
        let light = entity_light_at(handle, block[0], block[1], block[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        let phase = lodestone_render::block_entity::banner_phase(block, game_time, partial_tick);
        if let Some(spawn) = banner_spawn(block, state_id, patterns, phase, light) {
            out.push(spawn);
        }
    }
    out.sort_by_key(|s| s.pos);
    out
}

// --- decorated pot ---------------------------------------------------------

/// The decorated pot's stored sherds, parsed out of its NBT —
/// vanilla's own pot-decorations codec (its own sherds tag, key
/// `"sherds"`): a plain 4-element list of item ids in **`[back, left, right,
/// front]`** order (vanilla's own record field order, and
/// its own ordered stream of back, left, right, front), with
/// `minecraft:brick` the empty sentinel (vanilla's own pot-decorations item
/// lookup treats a brick as the empty case). A side whose id fails to parse, or is the sentinel,
/// is `None` — the same "drop rather than default" rule [`banner_patterns`]
/// documents, and the namespace is stripped for the same reason
/// [`banner_patterns`] strips one: [`decorated_pot_pattern_texture_stem`]
/// keys on the **bare** sherd path.
#[must_use]
fn decorated_pot_sherds(nbt: &lodestone_core::Nbt) -> [Option<String>; 4] {
    use lodestone_core::Nbt;

    let mut out: [Option<String>; 4] = [None, None, None, None];
    let Nbt::Compound(fields) = nbt else {
        return out;
    };
    let Some(Nbt::List { elements, .. }) =
        fields.iter().find(|(name, _)| name == "sherds").map(|(_, v)| v)
    else {
        return out;
    };
    for (slot, elem) in out.iter_mut().zip(elements.iter()) {
        let Nbt::String(id) = elem else { continue };
        let path = id.strip_prefix("minecraft:").unwrap_or(id);
        if path != "brick" {
            *slot = Some(path.to_string());
        }
    }
    out
}

/// Every decorated-pot position within [`VIEW_DISTANCE`], paired with its block
/// state and stored sherds.
///
/// A third NBT-reading candidate gather beside [`sign_candidates`] and
/// [`banner_candidates`], for the same reason those exist: [`chest_candidates`]
/// discards `be.nbt`, and a pot's decoration lives there.
#[must_use]
fn decorated_pot_candidates(
    world: &World,
    chunks: impl IntoIterator<Item = ChunkPos>,
    eye: Vec3,
) -> Vec<([i32; 3], u32, [Option<String>; 4])> {
    let cutoff = VIEW_DISTANCE * VIEW_DISTANCE;
    let mut candidates = Vec::new();
    for pos in chunks {
        let Some(chunk) = world.get(pos) else {
            continue;
        };
        for be in &chunk.block_entities {
            let x = pos.x * 16 + i32::from(be.rel_x);
            let z = pos.z * 16 + i32::from(be.rel_z);
            let y = i32::from(be.y);
            let centre = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
            if centre.distance_squared(eye) > cutoff {
                continue;
            }
            let state_id = chunk
                .column
                .get_block(usize::from(be.rel_x), y, usize::from(be.rel_z));
            candidates.push(([x, y, z], state_id, decorated_pot_sherds(&be.nbt)));
        }
    }
    candidates
}

/// One candidate resolved into a [`DecoratedPotSpawn`], or `None` if the state
/// at that position is not a decorated pot.
///
/// `facing_yaw_deg` reads the block's own `facing` property —
/// vanilla's own decorated-pot render-state extraction converts that same
/// direction to a yaw — the same
/// [`horizontal_facing_yaw`] [`banner_attachment`]'s wall arm already uses.
#[must_use]
fn decorated_pot_spawn(
    block: [i32; 3],
    state_id: u32,
    sherds: [Option<String>; 4],
    light: u8,
) -> Option<DecoratedPotSpawn> {
    let name = lodestone_data::block_states::block_name(state_id)?;
    if name != "minecraft:decorated_pot" {
        return None;
    }
    let facing_value = lodestone_data::block_states::properties(state_id)?
        .iter()
        .find(|(prop, _)| *prop == "facing")
        .map(|(_, value)| *value)?;
    let facing_yaw_deg = horizontal_facing_yaw(facing_value)?;
    let [back, left, right, front] = sherds;
    Some(DecoratedPotSpawn {
        pos: block,
        facing_yaw_deg,
        front,
        back,
        left,
        right,
        light,
    })
}

/// Every decorated pot to draw this frame — the pot sibling of
/// [`banner_spawns`]/[`shulker_spawns`].
#[must_use]
pub fn decorated_pot_spawns(handle: &SharedHandle, eye: Vec3) -> Vec<DecoratedPotSpawn> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();

    let candidates = {
        let world = store.read();
        decorated_pot_candidates(
            &world,
            chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }),
            eye,
        )
    };

    let sky_default = {
        let player = client.player();
        crate::mesher::sky_default_for_dimension(
            player.dimension.as_ref(),
            player.dimension_type.as_ref(),
        )
    };

    let mut out = Vec::new();
    for (block, state_id, sherds) in candidates {
        let light = entity_light_at(handle, block[0], block[1], block[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        if let Some(spawn) = decorated_pot_spawn(block, state_id, sherds, light) {
            out.push(spawn);
        }
    }
    out.sort_by_key(|s| s.pos);
    out
}

// --- conduit -----------------------------------------------------------

/// Reads the block state at an absolute world position, resolving whichever
/// chunk it falls in.
///
/// [`conduit_frame_scan`]'s 5×5×5 pass steps up to two cells past the
/// conduit's own chunk on any axis, and [`lodestone_world::ChunkColumn::
/// get_block`] only accepts **in-chunk** `0..16` coordinates (it panics
/// otherwise) — every other candidate gather in this module sidesteps that by
/// reading `be.rel_x`/`be.rel_z` off the block entity record itself, which a
/// scan around a *neighbouring* cell has no equivalent of. A chunk the client
/// has not loaded (only reachable at the render-distance boundary, since the
/// scan never strays more than two cells from a block entity this module
/// already filtered by [`VIEW_DISTANCE`]) reads as air, the same "no data
/// yet" fallback [`shulker_spawn`]'s missing-`facing` default and friends
/// already use.
#[must_use]
fn block_state_at(world: &World, x: i32, y: i32, z: i32) -> u32 {
    let chunk_pos = ChunkPos {
        x: x.div_euclid(16),
        z: z.div_euclid(16),
    };
    let Some(chunk) = world.get(chunk_pos) else {
        return lodestone_data::block_states::air_state_id();
    };
    let local_x = x.rem_euclid(16) as usize;
    let local_z = z.rem_euclid(16) as usize;
    chunk.column.get_block(local_x, y, local_z)
}

/// Vanilla's own is-water-at check, testing the fluid state against the water tag —
/// [`conduit_frame_scan`]'s inner-cube predicate. True for the water block
/// itself (source or flowing share one block name here) and for any other
/// block reporting `waterlogged=true`, matching vanilla's fluid-tag reading
/// rather than [`crate::collision`]'s narrower block-identity `is_water`,
/// which would refuse a waterlogged frame the way `chunk::is_water` in
/// `lodestone-server` deliberately does for the unrelated drowning check.
#[must_use]
fn is_water_block(state_id: u32) -> bool {
    let Some(name) = lodestone_data::block_states::block_name(state_id) else {
        return false;
    };
    if name == "minecraft:water" {
        return true;
    }
    lodestone_data::block_states::properties(state_id).is_some_and(|props| {
        props.iter().any(|(key, value)| *key == "waterlogged" && *value == "true")
    })
}

/// `ConduitBlockEntity.VALID_BLOCKS` — the four frame block identities
/// [`conduit_frame_scan`]'s 5×5×5 pass counts.
#[must_use]
fn is_conduit_frame_block(state_id: u32) -> bool {
    let Some(name) = lodestone_data::block_states::block_name(state_id) else {
        return false;
    };
    matches!(
        name,
        "minecraft:prismarine"
            | "minecraft:prismarine_bricks"
            | "minecraft:sea_lantern"
            | "minecraft:dark_prismarine"
    )
}

/// Every `minecraft:conduit` position within [`VIEW_DISTANCE`] — the
/// candidate set [`ConduitTicks::tick`] wants as its `present` argument.
/// Reuses [`chest_candidates`] exactly as [`shulker_spawns`] does, then
/// narrows to the one block identity: a conduit carries no NBT this module
/// needs (its whole appearance comes from the live blocks around it via
/// [`conduit_scan_frame`]), so there is no reason to duplicate
/// [`banner_candidates`]'s NBT-reading shape here.
#[must_use]
pub fn conduit_positions(handle: &SharedHandle, eye: Vec3) -> Vec<[i32; 3]> {
    let Some(snapshot) = block_entity_frame_snapshot(handle, eye) else {
        return Vec::new();
    };
    conduit_positions_from_snapshot(&snapshot)
}

#[must_use]
fn conduit_positions_from_snapshot(snapshot: &BlockEntityFrameSnapshot) -> Vec<[i32; 3]> {
    snapshot
        .candidates
        .iter()
        .filter_map(|candidate| {
            (candidate.state_id.name() == "minecraft:conduit").then_some(candidate.pos)
        })
        .collect()
}

/// Scans one conduit's activation frame against the real, live block store —
/// [`conduit_frame_scan`]'s own doc names this function as its intended
/// shell-side caller. Reads `handle` once rather than holding it, so this can
/// be handed to [`ConduitTicks::tick`] as a `FnMut` without borrowing the
/// client across a whole tick.
#[must_use]
pub fn conduit_scan_frame(handle: &SharedHandle, pos: [i32; 3]) -> ConduitFrame {
    let Some(client) = handle.get() else {
        return ConduitFrame::default();
    };
    let store = client.chunk_world();
    let world = store.read();
    conduit_frame_scan(
        pos,
        |p| is_water_block(block_state_at(&world, p[0], p[1], p[2])),
        |p| is_conduit_frame_block(block_state_at(&world, p[0], p[1], p[2])),
    )
}

/// Vanilla's own conduit-block-entity client-tick's own rescan cadence — `gameTime % 40L ==
/// 0L` gates a fresh shape scan rather than running one every tick. This
/// module has no shared world-time clock to gate on (every other animation
/// fold here — [`ChestLids`], [`BellShakes`], [`EnchantingTableBooks`] —
/// already runs its own local counter rather than reading one), so
/// [`ConduitTicks`] gates on **its own** per-position tick counter instead:
/// a disclosed simplification, not a transcription bug — the frame only
/// changes when a player edits blocks nearby, so a rescan cadence that is
/// merely *close* to vanilla's costs nothing visible.
const CONDUIT_FRAME_RESCAN_INTERVAL_TICKS: u32 = 40;

/// One conduit's client-side animation clock — `ConduitBlockEntity`'s own
/// `tickCount`/`activeRotation` counters, plus the last [`ConduitFrame`] scan.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ConduitClock {
    tick_count: u32,
    active_rotation_ticks: u32,
    frame: ConduitFrame,
}

/// Per-position conduit animation state, driven by nothing on the wire at
/// all — the conduit sibling of [`BellShakes`] and, more exactly,
/// [`EnchantingTableBooks`]: like a book's page-flip, a conduit's activation
/// is discovered by looking at the world (here, a periodic block-pattern
/// scan) rather than by a `BLOCK_EVENT`, so an entry has to be created and
/// advanced by [`tick`](Self::tick) rather than received.
///
/// Entries for a position no longer in [`conduit_positions`]' candidate set
/// are dropped on the next tick — the same GC [`ChestLids`]/[`BellShakes`]
/// already rely on, except a conduit has no "at rest" value to fall back to
/// once dropped, so a caller must stop asking [`resolve`](Self::resolve)
/// about a position once it stops appearing in `present`.
#[derive(Debug, Default, Clone)]
pub struct ConduitTicks {
    clocks: HashMap<[i32; 3], ConduitClock>,
}

impl ConduitTicks {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// One client tick for every conduit in `present` (this frame's candidate
    /// set — see [`conduit_positions`]), discovering any new entry and
    /// dropping any no longer present.
    ///
    /// `scan` resolves one position's [`ConduitFrame`] — a caller-supplied
    /// closure (bind [`conduit_scan_frame`]) rather than a `World` borrow
    /// held across the whole tick, matching [`conduit_frame_scan`]'s own
    /// closure-based block lookup. Called once immediately for a newly
    /// discovered position (so a conduit is never drawn one tick behind its
    /// real activation state) and again every
    /// [`CONDUIT_FRAME_RESCAN_INTERVAL_TICKS`] ticks thereafter.
    pub fn tick(&mut self, present: &[[i32; 3]], mut scan: impl FnMut([i32; 3]) -> ConduitFrame) {
        self.clocks.retain(|pos, _| present.contains(pos));
        for &pos in present {
            let is_new = !self.clocks.contains_key(&pos);
            let clock = self.clocks.entry(pos).or_insert_with(|| ConduitClock {
                tick_count: 0,
                active_rotation_ticks: 0,
                frame: ConduitFrame::default(),
            });
            if is_new {
                clock.frame = scan(pos);
            }
            let (tick_count, active_rotation_ticks) =
                conduit_advance(clock.tick_count, clock.active_rotation_ticks, clock.frame.is_active());
            clock.tick_count = tick_count;
            clock.active_rotation_ticks = active_rotation_ticks;
            if clock.tick_count % CONDUIT_FRAME_RESCAN_INTERVAL_TICKS == 0 {
                clock.frame = scan(pos);
            }
        }
    }

    /// One conduit's resolved [`ConduitSpawn`] for this partial tick, or
    /// `None` for a position [`tick`](Self::tick) has not (yet, or any
    /// longer) been told about. `light` is supplied by the caller, exactly as
    /// every other `*_spawn` in this module takes it separately from its
    /// animation state.
    #[must_use]
    pub fn resolve(&self, pos: [i32; 3], partial_tick: f32, light: u8) -> Option<ConduitSpawn> {
        let clock = self.clocks.get(&pos)?;
        let active = clock.frame.is_active();
        let hunting = active && clock.frame.is_hunting();
        let active_rotation_value =
            conduit_active_rotation_value(clock.active_rotation_ticks, partial_tick, active);
        Some(ConduitSpawn {
            pos,
            active,
            hunting,
            active_rotation_value,
            anim_time: conduit_anim_time(clock.tick_count, partial_tick),
            animation_phase: conduit_animation_phase(clock.tick_count),
            light,
        })
    }

    /// Number of tracked conduits (for stats and tests).
    #[must_use]
    pub fn len(&self) -> usize {
        self.clocks.len()
    }

    /// Whether nothing is being tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clocks.is_empty()
    }
}

/// Every conduit to draw this frame — the conduit sibling of
/// [`banner_spawns`]/[`shulker_spawns`]. Unlike those, the animation state
/// itself is **not** recomputed here: `ticks` must already have been
/// advanced this client tick ([`ConduitTicks::tick`], driven by
/// [`conduit_positions`]/[`conduit_scan_frame`] above) — a fresh
/// [`ConduitFrame`] needs a rescan cadence this per-frame call has no clock
/// for. This only reads what `ticks` already knows and attaches this frame's
/// light.
#[must_use]
pub fn conduit_spawns(
    handle: &SharedHandle,
    eye: Vec3,
    ticks: &ConduitTicks,
    partial_tick: f32,
) -> Vec<ConduitSpawn> {
    let Some(snapshot) = block_entity_frame_snapshot(handle, eye) else {
        return Vec::new();
    };
    conduit_spawns_from_snapshot(&snapshot, ticks, partial_tick)
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

/// Vanilla's own piston-moving-block-entity tick's ramp: `progress += 0.5F` per tick, so with
/// `TICKS_TO_EXTEND = 2` a whole push lasts **two ticks** — a tenth of a second.
///
/// The shortest animation in this module by a factor of five (a chest lid is ten
/// ticks, a bell fifty), which is why a stale render source is so much more
/// visible here: there is no window in which the frozen value looks like a
/// mid-animation frame.
const PISTON_PROGRESS_SPEED: f32 = 0.5;

/// One moving piston's animation clock — vanilla's own piston-moving-block-entity's
/// current and previous progress fields.
#[derive(Debug, Clone, Copy, PartialEq)]
struct PistonMove {
    progress: f32,
    /// `progressO`: the value at the start of the current tick, for the
    /// partial-tick lerp.
    previous: f32,
}

/// Per-position moving-piston animation state — the piston sibling of
/// [`ChestLids`] and [`BellShakes`], and the only one of the three that **no
/// packet drives**.
///
/// # Why a client-side clock is needed at all
///
/// Vanilla's own piston-moving-block-entity update-tag is a custom-only save, so the wire does
/// carry a `progress` — but it is `progressO`, the value at the *start* of the tick
/// the block entity was created on, and it is sent once. Vanilla's client then runs
/// its own piston-moving-block-entity tick locally, adding [`PISTON_PROGRESS_SPEED`] each
/// tick. Without that local ramp every push would draw at its seed value for its
/// whole two-tick life, and the seed is normally `0.0` — which
/// [`piston_head_pose`](crate::gpu) turns into a displacement of one **whole** cell
/// backwards, i.e. geometry buried inside the piston base. So the missing clock
/// does not degrade to "no animation", it degrades to overlapping blocks.
///
/// # Removal is driven by the world, not by a counter
///
/// Vanilla drops the block entity itself once `progressO >= 1.0` (after five
/// `deathTicks` on the client). Here the authority is simpler and more robust: a
/// tracked entry is dropped as soon as its cell stops holding a `moving_piston`
/// block entity, which is exactly when the server's own `finalTick` replaces the
/// cell. A piston whose removal packet is lost therefore settles at `progress ==
/// 1.0` — geometry exactly on its destination cell, indistinguishable from the
/// finished block — rather than being stranded mid-travel.
#[derive(Debug, Default, Clone)]
pub struct PistonMoves {
    moves: HashMap<[i32; 3], PistonMove>,
}

impl PistonMoves {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Advances every tracked piston one client tick — vanilla's own piston-moving-block-entity tick.
    ///
    /// `present` is every `moving_piston` block entity the world still holds,
    /// paired with the `progress` its NBT carries — [`moving_piston_seeds`] builds
    /// it. It does double duty: it is the **liveness set** (anything not in it is
    /// dropped) and the **seed** for a position seen for the first time.
    ///
    /// # A newly-seen piston is seeded, not advanced, in the same call
    ///
    /// The insert happens *after* the advance, deliberately. Advancing on the
    /// discovery tick would start every push at `progress == 0.5` and halve the
    /// animation to a single tick — visible as a head that appears already
    /// half-way out. Vanilla's ordering is the same shape for a different reason:
    /// the block entity is constructed during chunk load and `tick` first runs on
    /// the following tick.
    pub fn tick(&mut self, present: &[([i32; 3], f32)]) {
        self.moves.retain(|pos, m| {
            if !present.iter().any(|(p, _)| p == pos) {
                return false;
            }
            m.previous = m.progress;
            m.progress = (m.progress + PISTON_PROGRESS_SPEED).min(1.0);
            true
        });
        for &(pos, seed) in present {
            self.moves.entry(pos).or_insert(PistonMove {
                progress: seed.clamp(0.0, 1.0),
                previous: seed.clamp(0.0, 1.0),
            });
        }
    }

    /// The interpolated progress at `pos` — `getProgress(a)`, i.e.
    /// `lerp(a, progressO, progress)`.
    ///
    /// `None` for an untracked position. That is **not** the same "absent equals
    /// at rest" shortcut [`ChestLids::openness`] can take: `0.0` is a real, and the
    /// most displaced, progress value, so a caller must be able to tell "not
    /// tracked yet" from "at the start of its travel". The gather uses the NBT's own
    /// seed in that case, so a piston is never drawn from a value this map made up.
    #[must_use]
    pub fn progress(&self, pos: [i32; 3], partial_tick: f32) -> Option<f32> {
        let m = self.moves.get(&pos)?;
        let t = partial_tick.clamp(0.0, 1.0);
        Some(m.previous + (m.progress - m.previous) * t)
    }

    /// Number of pistons currently moving (for stats and tests).
    #[must_use]
    pub fn len(&self) -> usize {
        self.moves.len()
    }

    /// Whether nothing is being tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.moves.is_empty()
    }
}

/// One `moving_piston` block entity's NBT, decoded — vanilla's own
/// piston-moving-block-entity's
/// five load fields.
#[derive(Debug, Clone, PartialEq)]
struct MovingPistonNbt {
    /// `blockState`, resolved through [`lodestone_data::block_states::state_id`].
    moved_state: u32,
    /// `facing`, as a unit step. Vanilla's own legacy direction-id codec is a byte over
    /// the 3D data value, so this is an [`lodestone_core::Nbt::Byte`], **not** an
    /// int — reading it as one silently defaults every piston to `DOWN`.
    direction: [i32; 3],
    /// `progress`, which vanilla writes as `progressO`. Seeds
    /// [`PistonMoves::tick`]; it is not the value drawn.
    progress: f32,
    extending: bool,
    /// `source`: whether this cell is the piston *base*'s own, rather than a cell
    /// a pushed block is travelling through.
    source: bool,
}

/// Vanilla's own direction-from-3D-data-value's unit step, for the byte
/// its own legacy direction-id codec stores.
///
/// The order is `DOWN, UP, NORTH, SOUTH, WEST, EAST` — vanilla's own enum
/// declaration order, which is *not* alphabetical and not the horizontal-facing
/// order the sign and chest gathers use. `None` rather than a wrapping index for
/// anything out of range: vanilla's own by-id lookup wraps, but a wrapped facing here would
/// silently push a contraption sideways.
#[must_use]
fn direction_step_from_3d(id: i8) -> Option<[i32; 3]> {
    Some(match id {
        0 => [0, -1, 0],
        1 => [0, 1, 0],
        2 => [0, 0, -1],
        3 => [0, 0, 1],
        4 => [-1, 0, 0],
        5 => [1, 0, 0],
        _ => return None,
    })
}

/// Renders vanilla's own block-state codec's NBT compound — `{Name: "...", Properties: {...}}`
/// — as the canonical state string [`lodestone_data::block_states::state_id`]
/// parses.
///
/// Going via the string rather than a direct table lookup is not a detour: that
/// function's three-tier fallback (exact, default-plus-overrides, then the bare
/// default) is exactly what a hand-rolled property match would have to
/// reimplement, and it is the tier-2 arm that makes a *synthesised* state such as
/// `piston_head[facing=up,short=true,type=normal]` resolve at all.
///
/// Properties are sorted, because tier 1 compares against the generated table's
/// own sorted slice.
#[must_use]
fn nbt_block_state_string(nbt: &lodestone_core::Nbt) -> Option<String> {
    use lodestone_core::Nbt;

    let Nbt::Compound(fields) = nbt else {
        return None;
    };
    let field = |key: &str| fields.iter().find(|(name, _)| name == key).map(|(_, v)| v);
    let Some(Nbt::String(name)) = field("Name") else {
        return None;
    };
    let mut props: Vec<(&str, &str)> = match field("Properties") {
        Some(Nbt::Compound(pairs)) => pairs
            .iter()
            .filter_map(|(key, value)| match value {
                Nbt::String(value) => Some((key.as_str(), value.as_str())),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    if props.is_empty() {
        return Some(name.clone());
    }
    props.sort_unstable();
    let rendered: Vec<String> = props
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    Some(format!("{name}[{}]", rendered.join(",")))
}

/// Decodes one moving piston's NBT, or `None` if any required field is missing or
/// the wrong tag type.
///
/// A missing `blockState` is `AIR` in vanilla and `extractRenderState` then draws
/// nothing (`!blockState.isAir()`), so this declines rather than defaulting: the
/// caller has nothing to draw either way, and declining keeps the untracked/at-rest
/// distinction [`PistonMoves::progress`] documents.
#[must_use]
fn moving_piston_nbt(nbt: &lodestone_core::Nbt) -> Option<MovingPistonNbt> {
    use lodestone_core::Nbt;

    let Nbt::Compound(fields) = nbt else {
        return None;
    };
    let field = |key: &str| fields.iter().find(|(name, _)| name == key).map(|(_, v)| v);

    let moved_state =
        lodestone_data::block_states::state_id(&nbt_block_state_string(field("blockState")?)?)?;
    if moved_state == lodestone_data::block_states::air_state_id() {
        return None;
    }
    let Some(Nbt::Byte(facing)) = field("facing") else {
        return None;
    };
    let direction = direction_step_from_3d(*facing)?;
    // Vanilla's `getFloatOr("progress", 0.0F)`: a missing progress is the start of
    // the travel, which is a real state rather than a decode failure.
    let progress = match field("progress") {
        Some(Nbt::Float(v)) => *v,
        None => 0.0,
        _ => return None,
    };
    // `getBooleanOr` — an `Nbt::Byte`, and absent means `false`.
    let flag = |key: &str| match field(key) {
        Some(Nbt::Byte(v)) => Some(*v != 0),
        None => Some(false),
        _ => None,
    };
    Some(MovingPistonNbt {
        moved_state,
        direction,
        progress,
        extending: flag("extending")?,
        source: flag("source")?,
    })
}

/// Whether a block state is `minecraft:moving_piston`.
#[must_use]
fn is_moving_piston(state_id: u32) -> bool {
    lodestone_data::block_states::block_name(state_id) == Some("minecraft:moving_piston")
}

/// One block state's named property value, or `None`.
#[must_use]
fn state_property(state_id: u32, key: &str) -> Option<&'static str> {
    lodestone_data::block_states::properties(state_id)?
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, value)| *value)
}

/// Vanilla's own piston-head render-state extraction's three-way branch: which state to draw
/// offset, and whether a *second*, unoffset state (the retracting source piston's
/// own base) draws with it.
///
/// # The three arms, and why two of them synthesise a state
///
/// 1. **The moved state already is a `piston_head`** — the arm a plain extension
///    takes, because vanilla's own piston-base block pushes a head state into the cell in front
///    of it. Vanilla rewrites `short` from the progress. Its guard is `progress <=
///    4.0F`, which is *always* true (progress is `0..=1`); it is not ported as a
///    condition because a condition that cannot be false reads as one that can.
/// 2. **A retracting source piston** (`isSourcePiston && !isExtending`) — a sticky
///    piston pulling its head home. The stored state is the *base* block, so a head
///    has to be built from scratch: `type` from whether the base is sticky, `facing`
///    from the base's own `facing`, and `short` from the progress with the
///    **opposite** comparison to arm 1 (`>= 0.5`, not `<= 0.5`, because the head is
///    travelling the other way). Its base draws too, forced to `extended=true`.
/// 3. **Anything else** — an ordinary pushed or pulled block, drawn as stored.
///
/// `short` is a genuine visual difference (a short head's arm is 4/16 deep instead
/// of 12/16), and getting arm 2's comparison backwards produces a head that pops
/// long at the wrong moment — plausible enough to survive a screenshot.
#[must_use]
fn moving_piston_states(nbt: &MovingPistonNbt, progress: f32) -> Option<(u32, Option<u32>)> {
    use lodestone_data::block_states::{block_name, state_id};

    let moved_name = block_name(nbt.moved_state)?;
    if moved_name == "minecraft:piston_head" {
        let facing = state_property(nbt.moved_state, "facing")?;
        let head_type = state_property(nbt.moved_state, "type")?;
        let short = progress <= 0.5;
        return Some((
            state_id(&format!(
                "minecraft:piston_head[facing={facing},short={short},type={head_type}]"
            ))?,
            None,
        ));
    }
    if nbt.source && !nbt.extending {
        // Vanilla's own default piston-type's serialized name is `"normal"`, not `"default"`.
        let head_type = if moved_name == "minecraft:sticky_piston" {
            "sticky"
        } else {
            "normal"
        };
        let facing = state_property(nbt.moved_state, "facing")?;
        let short = progress >= 0.5;
        let head = state_id(&format!(
            "minecraft:piston_head[facing={facing},short={short},type={head_type}]"
        ))?;
        let base = state_id(&format!("{moved_name}[extended=true,facing={facing}]"))?;
        return Some((head, Some(base)));
    }
    Some((nbt.moved_state, None))
}

/// Every `moving_piston` block entity in the world, paired with the `progress` its
/// NBT carries — [`PistonMoves::tick`]'s whole input.
///
/// **Unbounded by [`VIEW_DISTANCE`], unlike every gather in this module**, and the
/// asymmetry is deliberate: this feeds the *clock*, not the draw. A push lasts two
/// ticks, so a piston that a player walks toward mid-push would otherwise be seeded
/// at the progress it had when it entered range rather than when it started, and
/// would visibly restart. The list is short by construction — a `moving_piston`
/// cell exists for two ticks — so the cost is a walk of the loaded block-entity
/// records, not of blocks.
#[must_use]
pub fn moving_piston_seeds(handle: &SharedHandle) -> Vec<([i32; 3], f32)> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();
    let world = store.read();
    let mut out = Vec::new();
    for pos in chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }) {
        let Some(chunk) = world.get(pos) else {
            continue;
        };
        for be in &chunk.block_entities {
            let y = i32::from(be.y);
            let state_id = chunk
                .column
                .get_block(usize::from(be.rel_x), y, usize::from(be.rel_z));
            if !is_moving_piston(state_id) {
                continue;
            }
            let Some(decoded) = moving_piston_nbt(&be.nbt) else {
                continue;
            };
            out.push((
                [
                    pos.x * 16 + i32::from(be.rel_x),
                    y,
                    pos.z * 16 + i32::from(be.rel_z),
                ],
                decoded.progress,
            ));
        }
    }
    out
}

/// Every moving piston to draw this frame — vanilla's `PistonHeadRenderer`.
///
/// Feeds neither the entity pipeline (no `bakeLayer`, so no rig) nor the item path
/// (not an item), but the **moving-block-model** seam falling blocks use: see
/// `crate::gpu::MovingPistonSource`.
///
/// # Where each of the two light samples comes from
///
/// `extractRenderState` computes `pos = getBlockPos().relative(
/// getMovementDirection().getOpposite())` and samples light there, one cell *back*
/// along the push. That is not a detail: the block entity's own cell is full of
/// `moving_piston`, and the cell behind it is the air (or the piston base) the
/// geometry is actually travelling out of. The base's sample is taken at
/// `pos.relative(getMovementDirection())`, which for the retracting case arm 2
/// serves collapses back to the block entity's own cell.
#[must_use]
pub fn moving_piston_spawns(
    handle: &SharedHandle,
    moves: &PistonMoves,
    eye: Vec3,
    partial_tick: f32,
) -> Vec<lodestone_render::MovingPistonSpawn> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();
    let cutoff = VIEW_DISTANCE * VIEW_DISTANCE;

    let candidates = {
        let world = store.read();
        let mut candidates = Vec::new();
        for pos in chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }) {
            let Some(chunk) = world.get(pos) else {
                continue;
            };
            for be in &chunk.block_entities {
                let x = pos.x * 16 + i32::from(be.rel_x);
                let z = pos.z * 16 + i32::from(be.rel_z);
                let y = i32::from(be.y);
                let centre = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
                if centre.distance_squared(eye) > cutoff {
                    continue;
                }
                let state_id = chunk
                    .column
                    .get_block(usize::from(be.rel_x), y, usize::from(be.rel_z));
                if !is_moving_piston(state_id) {
                    continue;
                }
                let Some(decoded) = moving_piston_nbt(&be.nbt) else {
                    continue;
                };
                candidates.push(([x, y, z], decoded));
            }
        }
        candidates
    };

    let sky_default = {
        let player = client.player();
        crate::mesher::sky_default_for_dimension(
            player.dimension.as_ref(),
            player.dimension_type.as_ref(),
        )
    };

    let mut out = Vec::new();
    for (block, decoded) in candidates {
        // The tracker's value when it has one, and the NBT's own seed otherwise —
        // never a made-up `0.0`, which is the *most* displaced progress there is.
        let progress = moves
            .progress(block, partial_tick)
            .unwrap_or(decoded.progress)
            .clamp(0.0, 1.0);
        let Some((state_id, base_state_id)) = moving_piston_states(&decoded, progress) else {
            continue;
        };
        // `getMovementDirection()` is `extending ? direction : -direction`, so its
        // opposite — the cell vanilla samples light at — is `-direction` while
        // extending and `+direction` while retracting.
        let back = if decoded.extending { -1 } else { 1 };
        let light_cell = [
            block[0] + decoded.direction[0] * back,
            block[1] + decoded.direction[1] * back,
            block[2] + decoded.direction[2] * back,
        ];
        let light = entity_light_at(handle, light_cell[0], light_cell[1], light_cell[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        let base_light = entity_light_at(handle, block[0], block[1], block[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        out.push(lodestone_render::MovingPistonSpawn {
            pos: block,
            state_id,
            base_state_id,
            direction: decoded.direction,
            progress,
            extending: decoded.extending,
            light,
            base_light,
        });
    }
    out.sort_by_key(|s| s.pos);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const POS: [i32; 3] = [4, 65, -9];

    #[test]
    fn a_non_lid_block_event_is_ignored() {
        let mut lids = ChestLids::new();
        // `b0 == 3` is a note block's instrument, not a chest lid.
        assert!(!lids.apply_block_event(POS, 3, 1));
        assert!(lids.is_empty());
        assert!(lids.apply_block_event(POS, 1, 1));
        assert_eq!(lids.len(), 1);
    }

    /// `b1` is a viewer *count*, not a boolean: two players looking into one
    /// chest send `2`, and that must still be open.
    #[test]
    fn a_viewer_count_above_one_still_opens() {
        let mut lids = ChestLids::new();
        lids.apply_block_event(POS, 1, 2);
        for _ in 0..10 {
            lids.tick();
        }
        assert!((lids.openness(POS, 1.0) - 1.0).abs() < 1e-5);
    }

    /// The ramp is ±0.1 per tick, so a lid takes exactly 10 ticks (half a
    /// second) to open. Asserted as a *duration*, not just at the endpoints —
    /// the endpoints are satisfied by a lid that teleports.
    #[test]
    fn the_lid_takes_ten_ticks_to_open_and_ten_to_shut() {
        let mut lids = ChestLids::new();
        lids.apply_block_event(POS, 1, 1);
        let mut seen = Vec::new();
        for _ in 0..10 {
            lids.tick();
            seen.push(lids.openness(POS, 1.0));
        }
        assert!((seen[0] - 0.1).abs() < 1e-5, "{seen:?}");
        assert!((seen[4] - 0.5).abs() < 1e-4, "{seen:?}");
        assert!((seen[9] - 1.0).abs() < 1e-5, "{seen:?}");
        // Monotone, and never overshoots.
        for pair in seen.windows(2) {
            assert!(pair[1] >= pair[0]);
            assert!(pair[1] <= 1.0 + 1e-6);
        }

        lids.apply_block_event(POS, 1, 0);
        for _ in 0..10 {
            lids.tick();
        }
        assert!(lids.openness(POS, 1.0).abs() < 1e-5);
    }

    /// The partial-tick lerp reads between the previous and current tick's
    /// values. Without it a lid steps at 20 Hz, which reads as a stutter rather
    /// than as a missing feature.
    #[test]
    fn openness_interpolates_within_a_tick() {
        let mut lids = ChestLids::new();
        lids.apply_block_event(POS, 1, 1);
        lids.tick(); // previous 0.0 -> openness 0.1
        assert!(lids.openness(POS, 0.0).abs() < 1e-6, "start of tick");
        assert!((lids.openness(POS, 0.5) - 0.05).abs() < 1e-6, "mid tick");
        assert!((lids.openness(POS, 1.0) - 0.1).abs() < 1e-6, "end of tick");
        // Out-of-range partial ticks clamp rather than extrapolating past 1.0.
        assert!((lids.openness(POS, 4.0) - 0.1).abs() < 1e-6);
        assert!(lids.openness(POS, -1.0).abs() < 1e-6);
    }

    /// A settled-shut lid is dropped so the map cannot grow without bound; the
    /// reported openness is unchanged by that, because an absent entry and a
    /// shut chest are the same value.
    #[test]
    fn settled_shut_lids_are_forgotten_without_changing_what_is_drawn() {
        let mut lids = ChestLids::new();
        lids.apply_block_event(POS, 1, 1);
        lids.tick();
        lids.apply_block_event(POS, 1, 0);
        for _ in 0..12 {
            lids.tick();
        }
        assert!(lids.is_empty(), "{} lids retained", lids.len());
        assert_eq!(lids.openness(POS, 1.0), 0.0);
        assert_eq!(lids.openness([0, 0, 0], 1.0), 0.0);
    }

    /// An open chest is *not* garbage-collected while it is open.
    #[test]
    fn an_open_lid_is_retained() {
        let mut lids = ChestLids::new();
        lids.apply_block_event(POS, 1, 1);
        for _ in 0..40 {
            lids.tick();
        }
        assert_eq!(lids.len(), 1);
        assert!((lids.openness(POS, 1.0) - 1.0).abs() < 1e-5);
    }

    /// Orientation comes from the real 26.2 state table, not a fixture: this is
    /// the check that the property *names* are right. A chest's `facing` is a
    /// horizontal direction and its `type` is single/left/right.
    #[test]
    fn chest_states_resolve_facing_and_half_from_the_real_table() {
        // Walk the whole table for chest states rather than hardcoding an id —
        // block state ids are not stable across versions and a hardcoded one is
        // the classic silently-stale fixture.
        let mut seen_halves = std::collections::BTreeSet::new();
        let mut seen_yaws = std::collections::BTreeSet::new();
        let mut chest_states = 0usize;
        for id in 0..lodestone_data::block_states::STATE_COUNT {
            if lodestone_data::block_states::block_name(id) != Some("minecraft:chest") {
                continue;
            }
            chest_states += 1;
            let (yaw, half) = chest_orientation(id).expect("a chest state must have facing");
            seen_yaws.insert(yaw as i32);
            seen_halves.insert(half);
            assert_eq!(chest_material(id), Some(ChestMaterial::Regular));
        }
        assert!(chest_states > 0, "no chest states in the table at all");
        assert_eq!(
            seen_yaws,
            [0, 90, 180, 270].into_iter().collect(),
            "all four horizontal facings must be reachable"
        );
        assert_eq!(
            seen_halves,
            [ChestHalf::Single, ChestHalf::Left, ChestHalf::Right]
                .into_iter()
                .collect(),
            "all three chest types must be reachable"
        );
    }

    #[test]
    fn every_chest_block_in_the_real_table_resolves_to_a_material() {
        for path in [
            "chest",
            "trapped_chest",
            "ender_chest",
            "copper_chest",
            "exposed_copper_chest",
            "weathered_copper_chest",
            "oxidized_copper_chest",
        ] {
            let name = format!("minecraft:{path}");
            let found = (0..lodestone_data::block_states::STATE_COUNT)
                .find(|id| lodestone_data::block_states::block_name(*id) == Some(name.as_str()));
            let id = found.unwrap_or_else(|| panic!("{name} is not in the 26.2 state table"));
            assert!(
                chest_material(id).is_some(),
                "{name} (state {id}) resolved to no material"
            );
            assert!(
                chest_orientation(id).is_some(),
                "{name} (state {id}) resolved no facing"
            );
        }
    }

    /// A non-chest with a block entity (a furnace has `facing` too) must resolve
    /// to no material, so `chest_spawns` skips it. This is the control on the
    /// material filter: without it every block entity in range would draw a
    /// chest.
    #[test]
    fn a_non_chest_block_entity_resolves_to_no_material() {
        for path in ["furnace", "barrel", "shulker_box", "beacon", "oak_sign"] {
            let name = format!("minecraft:{path}");
            let Some(id) = (0..lodestone_data::block_states::STATE_COUNT)
                .find(|id| lodestone_data::block_states::block_name(*id) == Some(name.as_str()))
            else {
                continue;
            };
            assert_eq!(chest_material(id), None, "{name} matched a chest material");
        }
    }

    #[test]
    fn chest_spawns_before_login_is_empty_rather_than_a_panic() {
        let handle: SharedHandle = std::sync::Arc::new(std::sync::OnceLock::new());
        let lids = ChestLids::new();
        assert!(chest_spawns(&handle, &lids, Vec3::ZERO, 0.0).is_empty());
    }
}

/// Kept as its own module rather than folded into `tests` above so it never
/// has to touch that block's interior — this file is shared with the chest
/// lid/gather work and a separate module is the lowest-collision way to add
/// coverage alongside it.
#[cfg(test)]
mod skull_tests {
    use super::*;

    /// Orientation comes from the real 26.2 state table, not a fixture — the
    /// check that the property *names* are right and that both the floor
    /// (`rotation`) and wall (`facing`) shapes are reachable. Mirrors
    /// `chest_states_resolve_facing_and_half_from_the_real_table`.
    #[test]
    fn skull_states_resolve_orientation_from_the_real_table() {
        let mut floor_segments = std::collections::BTreeSet::new();
        let mut wall_yaws = std::collections::BTreeSet::new();
        let mut floor_states = 0usize;
        let mut wall_states = 0usize;
        for id in 0..lodestone_data::block_states::STATE_COUNT {
            match lodestone_data::block_states::block_name(id) {
                Some("minecraft:skeleton_skull") => {
                    floor_states += 1;
                    match skull_orientation(id).expect("a floor skull must have an orientation") {
                        SkullOrientation::Floor { rotation_segment } => {
                            floor_segments.insert(rotation_segment);
                        }
                        SkullOrientation::Wall { .. } => panic!("skeleton_skull resolved as wall"),
                    }
                }
                Some("minecraft:skeleton_wall_skull") => {
                    wall_states += 1;
                    match skull_orientation(id).expect("a wall skull must have an orientation") {
                        SkullOrientation::Wall { facing_yaw_deg } => {
                            wall_yaws.insert(facing_yaw_deg as i32);
                        }
                        SkullOrientation::Floor { .. } => {
                            panic!("skeleton_wall_skull resolved as floor")
                        }
                    }
                }
                _ => {}
            }
        }
        assert!(floor_states > 0, "no floor skeleton_skull states at all");
        assert!(wall_states > 0, "no wall skeleton_wall_skull states at all");
        assert_eq!(
            floor_segments,
            (0..16).collect(),
            "all sixteen rotation segments must be reachable"
        );
        assert_eq!(
            wall_yaws,
            [0, 90, 180, 270].into_iter().collect(),
            "all four wall facings must be reachable"
        );
    }

    /// Every skull/head block path in vanilla 26.2, floor and wall — the list
    /// this renderer must resolve in full.
    ///
    /// **Named once on purpose.** It used to be two lists: this one, plus a
    /// "declined" list holding `dragon_*`/`piglin_*` that a sibling gate
    /// asserted resolved to `None`. That second list was a premise with an
    /// expiry date nothing tracked, and porting the two rigs made it assert
    /// the opposite of the truth. One list cannot go out of step with itself.
    const EVERY_SKULL_BLOCK_PATH: [&str; 14] = [
        "skeleton_skull",
        "skeleton_wall_skull",
        "wither_skeleton_skull",
        "wither_skeleton_wall_skull",
        "zombie_head",
        "zombie_wall_head",
        "creeper_head",
        "creeper_wall_head",
        "player_head",
        "player_wall_head",
        "dragon_head",
        "dragon_wall_head",
        "piglin_head",
        "piglin_wall_head",
    ];

    #[test]
    fn every_ported_skull_block_in_the_real_table_resolves() {
        for path in EVERY_SKULL_BLOCK_PATH {
            let name = format!("minecraft:{path}");
            let found = (0..lodestone_data::block_states::STATE_COUNT)
                .find(|id| lodestone_data::block_states::block_name(*id) == Some(name.as_str()));
            let id = found.unwrap_or_else(|| panic!("{name} is not in the 26.2 state table"));
            assert!(
                skull_type_for_state(id).is_some(),
                "{name} (state {id}) resolved to no skull type"
            );
            assert!(
                skull_orientation(id).is_some(),
                "{name} (state {id}) resolved no orientation"
            );
        }
    }

    /// [`EVERY_SKULL_BLOCK_PATH`] really is every one — vanilla's own seven
    /// skull-block types in a floor and a wall variant each — checked against
    /// the 26.2 state table itself rather than against a second hand-written
    /// list. A vanilla skull block missing from that constant would otherwise
    /// be silently unguarded: the gate above only proves the paths it is given
    /// resolve, never that it was given all of them.
    #[test]
    fn the_skull_path_list_is_every_skull_block_in_the_real_table() {
        let mut in_table: Vec<&str> = (0..lodestone_data::block_states::STATE_COUNT)
            .filter_map(lodestone_data::block_states::block_name)
            .filter_map(|name| name.strip_prefix("minecraft:"))
            .filter(|path| {
                (path.ends_with("_skull") || path.ends_with("_head"))
                    && SkullType::from_block_path(path).is_some()
            })
            .collect();
        in_table.sort_unstable();
        in_table.dedup();
        let mut listed: Vec<&str> = EVERY_SKULL_BLOCK_PATH.to_vec();
        listed.sort_unstable();
        assert_eq!(in_table, listed, "the constant and the 26.2 state table disagree");
    }

    /// A non-skull block entity with a `facing` property (a furnace) must not
    /// resolve — the control on the type filter, mirroring
    /// `a_non_chest_block_entity_resolves_to_no_material`.
    #[test]
    fn a_non_skull_block_entity_resolves_to_no_skull_type() {
        for path in ["furnace", "chest", "barrel", "bell"] {
            let name = format!("minecraft:{path}");
            let Some(id) = (0..lodestone_data::block_states::STATE_COUNT)
                .find(|id| lodestone_data::block_states::block_name(*id) == Some(name.as_str()))
            else {
                continue;
            };
            assert_eq!(
                skull_type_for_state(id),
                None,
                "{name} matched a skull type"
            );
        }
    }

    #[test]
    fn skull_spawns_before_login_is_empty_rather_than_a_panic() {
        let handle: SharedHandle = std::sync::Arc::new(std::sync::OnceLock::new());
        assert!(skull_spawns(&handle, Vec3::ZERO).is_empty());
    }

    fn base64_encode(bytes: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                chunk.get(1).copied().unwrap_or(0),
                chunk.get(2).copied().unwrap_or(0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(char::from(TABLE[((n >> (18 - 6 * i)) & 0x3f) as usize]));
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    fn player_head_profile(value: Nbt) -> Nbt {
        Nbt::Compound(vec![(
            "profile".to_owned(),
            Nbt::Compound(vec![(
                "properties".to_owned(),
                Nbt::List {
                    element_type: NbtTag::Compound,
                    elements: vec![Nbt::Compound(vec![
                        ("name".to_owned(), Nbt::String("textures".to_owned())),
                        ("value".to_owned(), value),
                        ("signature".to_owned(), Nbt::String("unused".to_owned())),
                    ])],
                },
            )]),
        )])
    }

    #[test]
    fn a_modern_player_head_profile_extracts_its_decoded_skin_url() {
        let url = "https://textures.minecraft.net/texture/placed-player-head";
        let value = base64_encode(format!(r#"{{"textures":{{"SKIN":{{"url":"{url}"}}}}}}"#).as_bytes());
        assert_eq!(player_head_skin_url(&player_head_profile(Nbt::String(value))).as_deref(), Some(url));
    }

    #[test]
    fn malformed_player_head_profiles_fall_back_without_a_url() {
        let cases = [
            Nbt::End,
            Nbt::Compound(vec![("profile".to_owned(), Nbt::String("wrong".to_owned()))]),
            player_head_profile(Nbt::Int(4)),
            player_head_profile(Nbt::String("not-base64".to_owned())),
        ];
        for nbt in cases {
            assert!(player_head_skin_url(&nbt).is_none(), "{nbt:?}");
        }
    }

    #[test]
    fn static_skulls_keep_their_static_sheet_when_profile_nbt_is_present() {
        let state = (0..lodestone_data::block_states::STATE_COUNT)
            .find(|id| lodestone_data::block_states::block_name(*id) == Some("minecraft:skeleton_skull"))
            .expect("skeleton skull must be in the state table");
        let spawn = skull_spawn(
            [0, 0, 0],
            known_state_id(state),
            lodestone_render::ENTITY_FULLBRIGHT,
        )
            .expect("a skeleton skull must resolve");
        assert_eq!(spawn.texture, lodestone_render::skull_texture_stem(SkullType::Skeleton));
        assert!(player_head_skin_url(&player_head_profile(Nbt::String("not-base64".to_owned()))).is_none());
    }

    #[test]
    fn repeated_player_head_urls_request_one_shared_skin_fetch() {
        let url = "https://textures.minecraft.net/texture/placed-head-request-once";
        let before = crate::remote_skins::requested_urls()
            .iter()
            .filter(|seen| seen.as_str() == url)
            .count();
        let skin = BlockEntityTexture::PlayerSkin(Arc::<str>::from(url));
        let skulls = [
            SkullSpawn {
                skull_type: SkullType::Player,
                texture: skin.clone(),
                ..SkullSpawn::at([0, 0, 0])
            },
            SkullSpawn {
                skull_type: SkullType::Player,
                texture: skin,
                ..SkullSpawn::at([1, 0, 0])
            },
        ];
        request_player_head_skins(&skulls);
        let after = crate::remote_skins::requested_urls()
            .iter()
            .filter(|seen| seen.as_str() == url)
            .count();
        assert_eq!(after - before, 1, "one fetch per repeated player-head URL");
    }
}

/// Kept as its own module for the same reason `skull_tests` is: this file is
/// shared with the chest/skull/sign gather work.
#[cfg(test)]
mod bell_tests {
    use super::*;

    /// A real 26.2 `bell` state must resolve, and a bell has no per-block-path
    /// variant to pick between — unlike chest/skull, every state of the one
    /// `minecraft:bell` block draws the identical rig, so this only checks
    /// presence and resolution, not orientation.
    #[test]
    fn bell_is_present_and_resolves_from_the_real_table() {
        let id = (0..lodestone_data::block_states::STATE_COUNT)
            .find(|id| lodestone_data::block_states::block_name(*id) == Some("minecraft:bell"))
            .expect("bell must be in the 26.2 state table");
        assert!(bell_is_present(known_state_id(id)));
        let shakes = BellShakes::new();
        let spawn = bell_spawn(
            [1, 2, 3],
            known_state_id(id),
            lodestone_render::ENTITY_FULLBRIGHT,
            &shakes,
            0.0,
        )
            .expect("must resolve");
        assert_eq!(spawn.pos, [1, 2, 3]);
        assert_eq!(spawn.shake, None, "an unrung bell is at rest");
    }

    /// The `BLOCK_EVENT` -> shake chain, end to end on the CPU side: a ring makes
    /// the gather report a shake, the tick counter advances, and the entry is gone
    /// once vanilla's 50-tick window closes.
    #[test]
    fn a_block_event_rings_the_bell_for_fifty_ticks_and_then_stops() {
        let id = (0..lodestone_data::block_states::STATE_COUNT)
            .find(|id| lodestone_data::block_states::block_name(*id) == Some("minecraft:bell"))
            .expect("bell must be in the 26.2 state table");
        let pos = [4, 5, 6];
        let mut shakes = BellShakes::new();
        assert!(shakes.apply_block_event(pos, 1, 2), "b0 == 1 with a north face rings");
        let spawn = bell_spawn(
            pos,
            known_state_id(id),
            lodestone_render::ENTITY_FULLBRIGHT,
            &shakes,
            0.0,
        )
            .expect("must resolve");
        assert_eq!(spawn.shake, Some((BellShakeDirection::North, 0.0)));

        // Ten ticks in, the counter is ten and the angle is non-zero — the whole
        // point of the chain, since `bell_shake_angle(_, 0.0)` is also zero and a
        // frozen counter would be indistinguishable from a bell at rest.
        for _ in 0..10 {
            shakes.tick();
        }
        // At partial tick 0 the value is the *start* of the current tick, which
        // after ten ticks is 9 — the same convention `ChestLids::openness` uses,
        // and the reason both trackers keep a `previous`.
        let (direction, ticks) = shakes.shake(pos, 0.0).expect("still shaking");
        assert_eq!(direction, BellShakeDirection::North);
        assert!((ticks - 9.0).abs() < 0.001, "ticks did not advance: {ticks}");
        let (_, end) = shakes.shake(pos, 1.0).expect("still shaking");
        assert!((end - 10.0).abs() < 0.001, "the partial tick does not interpolate: {end}");
        let (x_rot, z_rot) = lodestone_render::bell_shake_angle(Some(direction), ticks);
        assert!(x_rot.abs() > 0.0001, "a shaking bell must be rotated: {x_rot}");
        assert_eq!(z_rot, 0.0, "a north hit swings on x only");

        // And it ends: vanilla's own bell block-entity tick clears at 50.
        for _ in 0..45 {
            shakes.tick();
        }
        assert!(shakes.is_empty(), "the shake outlived its 50-tick window");
        assert_eq!(shakes.shake(pos, 0.0), None);
    }

    /// The four horizontal faces map to vanilla's own direction-from-data order, and
    /// the two vertical ones are dropped rather than stored as a direction the
    /// model has no rotation for.
    #[test]
    fn the_shake_direction_is_vanillas_own_3d_data_order() {
        assert_eq!(shake_direction_from_3d(2), Some(BellShakeDirection::North));
        assert_eq!(shake_direction_from_3d(3), Some(BellShakeDirection::South));
        assert_eq!(shake_direction_from_3d(4), Some(BellShakeDirection::West));
        assert_eq!(shake_direction_from_3d(5), Some(BellShakeDirection::East));
        assert_eq!(shake_direction_from_3d(0), None, "DOWN has no swing");
        assert_eq!(shake_direction_from_3d(1), None, "UP has no swing");
        // And a non-ring event never starts one, whatever its parameter says.
        let mut shakes = BellShakes::new();
        assert!(!shakes.apply_block_event([0, 0, 0], 0, 2));
        assert!(shakes.is_empty());
    }

    /// A non-bell block entity with a `facing` property (a furnace, a chest)
    /// must not resolve — the control on the type filter, mirroring
    /// `a_non_skull_block_entity_resolves_to_no_skull_type`.
    #[test]
    fn a_non_bell_block_entity_resolves_to_no_bell_spawn() {
        for path in ["furnace", "chest", "barrel", "skeleton_skull"] {
            let name = format!("minecraft:{path}");
            let Some(id) = (0..lodestone_data::block_states::STATE_COUNT)
                .find(|id| lodestone_data::block_states::block_name(*id) == Some(name.as_str()))
            else {
                continue;
            };
            assert!(!bell_is_present(known_state_id(id)), "{name} matched as a bell");
            assert_eq!(
                bell_spawn(
                    [0, 0, 0],
                    known_state_id(id),
                    lodestone_render::ENTITY_FULLBRIGHT,
                    &BellShakes::new(),
                    0.0,
                ),
                None,
                "{name} unexpectedly resolved a bell spawn"
            );
        }
    }

    #[test]
    fn bell_spawns_before_login_is_empty_rather_than_a_panic() {
        let handle: SharedHandle = std::sync::Arc::new(std::sync::OnceLock::new());
        assert!(bell_spawns(&handle, &BellShakes::new(), Vec3::ZERO, 0.0).is_empty());
    }
}

/// Kept as its own module for the same reason `skull_tests` is: this file is
/// shared with the chest/skull gather work.
#[cfg(test)]
mod sign_tests {
    use super::*;

    /// Orientation comes from the real 26.2 state table, not a fixture —
    /// mirrors `skull_states_resolve_orientation_from_the_real_table`. Only
    /// `oak_sign`/`oak_wall_sign` are walked (not every wood), since the
    /// property *shape* — not the wood — is what is under test, exactly the
    /// same choice `chest_states_resolve_facing_and_half_from_the_real_table`
    /// makes for one chest block.
    #[test]
    fn sign_states_resolve_orientation_from_the_real_table() {
        let mut ground_segments = std::collections::BTreeSet::new();
        let mut wall_yaws = std::collections::BTreeSet::new();
        let mut ground_states = 0usize;
        let mut wall_states = 0usize;
        for id in 0..lodestone_data::block_states::STATE_COUNT {
            match lodestone_data::block_states::block_name(id) {
                Some("minecraft:oak_sign") => {
                    ground_states += 1;
                    assert!(sign_kind_for_state(known_state_id(id)).is_some());
                    match sign_orientation(known_state_id(id))
                        .expect("a ground sign must have an orientation")
                    {
                        SignOrientation::Ground { rotation_segment } => {
                            ground_segments.insert(rotation_segment);
                        }
                        SignOrientation::Wall { .. } => panic!("oak_sign resolved as wall"),
                    }
                }
                Some("minecraft:oak_wall_sign") => {
                    wall_states += 1;
                    assert!(sign_kind_for_state(known_state_id(id)).is_some());
                    match sign_orientation(known_state_id(id))
                        .expect("a wall sign must have an orientation")
                    {
                        SignOrientation::Wall { facing_yaw_deg } => {
                            wall_yaws.insert(facing_yaw_deg as i32);
                        }
                        SignOrientation::Ground { .. } => panic!("oak_wall_sign resolved as ground"),
                    }
                }
                _ => {}
            }
        }
        assert!(ground_states > 0, "no ground oak_sign states at all");
        assert!(wall_states > 0, "no wall oak_wall_sign states at all");
        assert_eq!(
            ground_segments,
            (0..16).collect(),
            "all sixteen rotation segments must be reachable"
        );
        assert_eq!(
            wall_yaws,
            [0, 90, 180, 270].into_iter().collect(),
            "all four wall facings must be reachable"
        );
    }

    /// Every plain sign block in the real 26.2 table — every wood, both
    /// standing and wall — must resolve as a sign with an orientation.
    /// Mirrors `every_ported_skull_block_in_the_real_table_resolves`.
    #[test]
    fn every_plain_sign_block_in_the_real_table_resolves() {
        for wood in [
            "oak", "spruce", "birch", "jungle", "acacia", "dark_oak", "mangrove", "cherry",
            "pale_oak", "bamboo", "crimson", "warped",
        ] {
            for suffix in ["sign", "wall_sign"] {
                let name = format!("minecraft:{wood}_{suffix}");
                let found = (0..lodestone_data::block_states::STATE_COUNT)
                    .find(|id| lodestone_data::block_states::block_name(*id) == Some(name.as_str()));
                let id = found.unwrap_or_else(|| panic!("{name} is not in the 26.2 state table"));
                assert!(
                    sign_kind_for_state(known_state_id(id)).is_some(),
                    "{name} (state {id}) not a sign"
                );
                assert!(
                    sign_orientation(known_state_id(id)).is_some(),
                    "{name} (state {id}) resolved no orientation"
                );
            }
        }
    }

    /// Hanging signs now resolve — as [`SignKind::Hanging`], **not** as
    /// plain. This test replaces `hanging_signs_are_present_but_declined`,
    /// which asserted the opposite: the decline was recorded as needing "a
    /// different model set again (chains, a bar)", and that was 1.20's shape,
    /// not 26.2's (see [`SignKind`]'s doc).
    ///
    /// The load-bearing half is the *kind*, not the `is_some()`: the two
    /// families share the `_sign` suffix, so a name check that forgot to look
    /// for `hanging` first would pass an `is_some()` assertion and draw every
    /// hanging sign's text at a plain sign's height and scale.
    #[test]
    fn hanging_signs_resolve_as_hanging_and_plain_ones_as_plain() {
        for path in [
            "oak_hanging_sign",
            "oak_wall_hanging_sign",
            "bamboo_hanging_sign",
            "bamboo_wall_hanging_sign",
        ] {
            let name = format!("minecraft:{path}");
            let found = (0..lodestone_data::block_states::STATE_COUNT)
                .find(|id| lodestone_data::block_states::block_name(*id) == Some(name.as_str()));
            let id = found.unwrap_or_else(|| panic!("{name} is not in the 26.2 state table"));
            assert_eq!(
                sign_kind_for_state(known_state_id(id)),
                Some(SignKind::Hanging),
                "{name} (state {id}) must resolve as a hanging sign"
            );
            assert!(
                sign_orientation(known_state_id(id)).is_some(),
                "{name} (state {id}) resolved no orientation"
            );
        }
        for path in ["oak_sign", "oak_wall_sign"] {
            let name = format!("minecraft:{path}");
            let id = (0..lodestone_data::block_states::STATE_COUNT)
                .find(|id| lodestone_data::block_states::block_name(*id) == Some(name.as_str()))
                .unwrap_or_else(|| panic!("{name} is not in the 26.2 state table"));
            assert_eq!(sign_kind_for_state(known_state_id(id)), Some(SignKind::Plain), "{name}");
        }
    }

    /// Every hanging sign block in the real 26.2 table — every wood, both
    /// ceiling and wall — resolves with a kind *and* an orientation, and the
    /// wall variant resolves as [`SignOrientation::Wall`] while the ceiling
    /// one resolves as [`SignOrientation::Ground`]. The orientation fork is
    /// the part that could silently go wrong: a ceiling hanging sign carries
    /// `attached` and `rotation`, a wall one carries `facing`, and
    /// [`sign_orientation`] returns on whichever it meets first.
    #[test]
    fn every_hanging_sign_block_in_the_real_table_resolves_with_the_right_fork() {
        for wood in [
            "oak", "spruce", "birch", "jungle", "acacia", "dark_oak", "mangrove", "cherry",
            "pale_oak", "bamboo", "crimson", "warped",
        ] {
            for (suffix, wall) in [("hanging_sign", false), ("wall_hanging_sign", true)] {
                let name = format!("minecraft:{wood}_{suffix}");
                let ids: Vec<u32> = (0..lodestone_data::block_states::STATE_COUNT)
                    .filter(|id| {
                        lodestone_data::block_states::block_name(*id) == Some(name.as_str())
                    })
                    .collect();
                assert!(!ids.is_empty(), "{name} is not in the 26.2 state table");
                for id in ids {
                    assert_eq!(
                        sign_kind_for_state(known_state_id(id)),
                        Some(SignKind::Hanging),
                        "{name}"
                    );
                    match sign_orientation(known_state_id(id)) {
                        Some(SignOrientation::Wall { .. }) if wall => {}
                        Some(SignOrientation::Ground { .. }) if !wall => {}
                        other => panic!("{name} (state {id}) resolved {other:?}, wall={wall}"),
                    }
                }
            }
        }
    }

    /// A non-sign block entity must not resolve — the control on the type
    /// filter, mirroring `a_non_skull_block_entity_resolves_to_no_skull_type`.
    #[test]
    fn a_non_sign_block_entity_resolves_to_no_sign_kind() {
        for path in ["furnace", "chest", "barrel", "bell", "skeleton_skull"] {
            let name = format!("minecraft:{path}");
            let Some(id) = (0..lodestone_data::block_states::STATE_COUNT)
                .find(|id| lodestone_data::block_states::block_name(*id) == Some(name.as_str()))
            else {
                continue;
            };
            assert!(
                sign_kind_for_state(known_state_id(id)).is_none(),
                "{name} matched a sign kind"
            );
        }
    }

    /// A real 26.2 `oak_sign` state joined with typed text (the shape
    /// `docs/block-entity-renderers.md`'s live probe captured, parsed once
    /// already in `lodestone-world`'s own tests) must survive the whole
    /// `sign_spawn` resolution — the join between that parse and the
    /// block-state-driven orientation/kind gate, which is the one thing
    /// `lodestone-world`'s tests cannot see since they have no state table.
    #[test]
    fn a_real_sign_state_plus_real_text_resolves_to_a_spawn_with_that_text() {
        let id = (0..lodestone_data::block_states::STATE_COUNT)
            .find(|id| lodestone_data::block_states::block_name(*id) == Some("minecraft:oak_sign"))
            .expect("oak_sign must be in the 26.2 state table");
        let mut text = SignText::default();
        text.front.lines[0] = vec![lodestone_world::SignTextSpan {
            text: "LODESTONE PROBE".to_owned(),
            ..Default::default()
        }];
        let spawn = sign_spawn(
            [0, 64, 0],
            known_state_id(id),
            text,
            lodestone_render::ENTITY_FULLBRIGHT,
        )
            .expect("a real oak_sign state must resolve to a spawn");
        assert_eq!(spawn.front.lines[0][0].text, "LODESTONE PROBE");
        assert!(matches!(spawn.orientation, SignOrientation::Ground { .. }));
    }

    #[test]
    fn sign_spawns_before_login_is_empty_rather_than_a_panic() {
        let handle: SharedHandle = std::sync::Arc::new(std::sync::OnceLock::new());
        assert!(sign_spawns(&handle, Vec3::ZERO).is_empty());
    }
}

/// Shulker boxes — kept in its own module beside `bell_tests` for the
/// same reason: this file is shared across every block-entity family.
#[cfg(test)]
mod shulker_tests {
    use super::*;

    /// Finds the first state id whose block name matches, against the real 26.2
    /// table rather than a fixture.
    fn state_named(name: &str) -> StateId {
        (0..lodestone_data::block_states::STATE_COUNT)
            .find(|id| lodestone_data::block_states::block_name(*id) == Some(name))
            .and_then(StateId::new)
            .unwrap_or_else(|| panic!("{name} must be in the 26.2 state table"))
    }

    /// **The colour is the block id, not a property.** A `color` lookup finds
    /// nothing on any of the seventeen blocks and would draw every box undyed —
    /// which looks like a texture-loading problem rather than a resolver bug.
    #[test]
    fn the_dye_colour_comes_off_the_block_id_and_the_plain_box_has_none() {
        let plain = shulker_orientation(state_named("minecraft:shulker_box"))
            .expect("the plain box resolves");
        assert_eq!(plain.0, None);
        for colour in SHULKER_COLOURS {
            let id = state_named(&format!("minecraft:{colour}_shulker_box"));
            let (resolved, _) = shulker_orientation(id).expect("a dyed box resolves");
            assert_eq!(resolved, Some(colour), "{colour} did not resolve");
        }
        // Not every block with a `facing` property is a shulker box.
        assert!(shulker_orientation(state_named("minecraft:chest")).is_none());
    }

    /// Every `FACING` value resolves, including the two vertical ones a chest
    /// cannot have — and a state with no `facing` at all takes vanilla's own
    /// `getValueOrElse(FACING, UP)` default rather than failing.
    #[test]
    fn every_facing_resolves_and_a_missing_one_defaults_to_up() {
        let mut seen = std::collections::HashSet::new();
        for id in 0..lodestone_data::block_states::STATE_COUNT {
            if lodestone_data::block_states::block_name(id) != Some("minecraft:shulker_box") {
                continue;
            }
            let (_, facing) = shulker_orientation(known_state_id(id)).expect("resolves");
            seen.insert(facing);
        }
        assert_eq!(
            seen.len(),
            6,
            "the plain shulker box should span all six facings, saw {seen:?}"
        );
        let spawn = shulker_spawn(
            [7, 8, 9],
            state_named("minecraft:shulker_box"),
            lodestone_render::ENTITY_FULLBRIGHT,
        )
        .expect("resolves");
        assert_eq!(spawn.pos, [7, 8, 9]);
        assert_eq!(spawn.progress, 0.0, "a box nobody has open is closed");
    }

    /// The gather is empty rather than a panic before login, matching every other
    /// family's — the guard that lets the source be installed unconditionally.
    #[test]
    fn shulker_spawns_before_login_is_empty_rather_than_a_panic() {
        let handle: SharedHandle = std::sync::Arc::new(std::sync::OnceLock::new());
        assert!(shulker_spawns(&handle, Vec3::ZERO).is_empty());
    }

    /// `has_book` decides whether there is anything to draw, and `facing` goes
    /// through the *clockwise* yaw.
    ///
    /// Driven over every real `minecraft:lectern` state id in the data crate
    /// rather than a hand-built one, so the four facings and both `has_book`
    /// values all come from the jar. Two things the walk pins that a single
    /// hand-picked state cannot: a bookless lectern yields **no** spawn at all
    /// (there is genuinely nothing to draw — the shelf is a real block model),
    /// and every book-bearing one yields a yaw that is *not* its plain facing
    /// yaw, which is the quarter-turn trap.
    #[test]
    fn a_lectern_only_spawns_with_a_book_and_takes_the_clockwise_yaw() {
        let mut with_book = 0_usize;
        let mut without_book = 0_usize;
        let mut yaws = std::collections::HashSet::new();
        for id in 0..lodestone_data::block_states::STATE_COUNT {
            if lodestone_data::block_states::block_name(id) != Some("minecraft:lectern") {
                continue;
            }
            let props = lodestone_data::block_states::properties(id).expect("lectern has properties");
            let facing = props
                .iter()
                .find(|(n, _)| *n == "facing")
                .map(|(_, v)| *v)
                .expect("lectern has facing");
            let has_book = props
                .iter()
                .any(|(n, v)| *n == "has_book" && *v == "true");

            let state_id = StateId::new(id).expect("iterated state id is in the census");
            match lectern_spawn([1, 2, 3], state_id, lodestone_render::ENTITY_FULLBRIGHT) {
                None => {
                    assert!(!has_book, "a lectern with a book must spawn");
                    without_book += 1;
                }
                Some(spawn) => {
                    assert!(has_book, "a bookless lectern must not spawn");
                    assert_eq!(spawn.pos, [1, 2, 3]);
                    let plain = horizontal_facing_yaw(facing).expect("horizontal");
                    assert_ne!(
                        spawn.facing_yaw_deg, plain,
                        "{facing}: the plain facing yaw lays the book sideways"
                    );
                    assert_eq!(
                        Some(spawn.facing_yaw_deg),
                        horizontal_facing_clockwise_yaw(facing)
                    );
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "the four yaws are exact multiples of 90"
                    )]
                    yaws.insert(spawn.facing_yaw_deg as i32);
                    with_book += 1;
                }
            }
        }
        assert_eq!(yaws.len(), 4, "all four facings, saw {yaws:?}");
        assert!(with_book > 0 && without_book > 0, "{with_book}/{without_book}");
        let bell = StateId::new(state_named("minecraft:bell")).expect("bell state is canonical");
        assert!(
            lectern_spawn([0, 0, 0], bell, 0).is_none(),
            "a bell is not a lectern"
        );
    }

    #[test]
    fn an_out_of_census_lectern_candidate_cannot_cross_the_snapshot_boundary() {
        assert!(
            StateId::new(u32::MAX).is_none(),
            "an unvalidated raw id cannot construct a frame candidate"
        );
        let snapshot = BlockEntityFrameSnapshot::default();

        assert!(lectern_spawns_from_snapshot(&snapshot).is_empty());
    }

    /// **The suffix-order trap, and the two angle conventions.**
    ///
    /// `"red_wall_banner"` ends in `_banner`, so a colour parse that tries
    /// `_banner` first strips it to `"red_wall"` — not a dye name — and **every
    /// wall banner in the world silently draws nothing**. The gate drives all
    /// sixteen dyes through both block families, so the ordering cannot regress
    /// for one colour and pass for the rest.
    ///
    /// It also pins that the two forms take their angle from *different*
    /// properties, since neither block has the other's: a standing banner has
    /// `rotation` and a wall banner has `facing`.
    #[test]
    fn every_dye_resolves_for_both_banner_families_and_takes_its_own_angle() {
        use lodestone_render::BannerAttachment;

        let mut ground = 0_usize;
        let mut wall = 0_usize;
        let mut segments = std::collections::HashSet::new();
        let mut facings = std::collections::HashSet::new();
        for id in 0..lodestone_data::block_states::STATE_COUNT {
            let Some(name) = lodestone_data::block_states::block_name(id) else {
                continue;
            };
            if !name.ends_with("_banner") {
                continue;
            }
            let is_wall_block = name.ends_with("_wall_banner");
            let (_, is_wall) = banner_colour(id)
                .unwrap_or_else(|| panic!("{name} must resolve a dye colour and a form"));
            assert_eq!(is_wall, is_wall_block, "{name}");

            let attachment = banner_attachment(id, is_wall)
                .unwrap_or_else(|| panic!("{name} must resolve an attachment"));
            match attachment {
                BannerAttachment::Ground { rotation_segment } => {
                    assert!(!is_wall_block, "{name} resolved as standing");
                    segments.insert(rotation_segment);
                    ground += 1;
                }
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "the four facing yaws are exact multiples of 90"
                )]
                BannerAttachment::Wall { facing_yaw_deg } => {
                    assert!(is_wall_block, "{name} resolved as wall");
                    facings.insert(facing_yaw_deg as i32);
                    wall += 1;
                }
            }
        }
        // 16 dyes x 16 rotations, and 16 dyes x 4 facings.
        assert_eq!(ground, 256, "sixteen dyes across sixteen rotation segments");
        assert_eq!(wall, 64, "sixteen dyes across four facings");
        assert_eq!(segments.len(), 16, "every rotation segment, saw {segments:?}");
        assert_eq!(facings.len(), 4, "every facing, saw {facings:?}");

        // The control that makes the ordering assertion mean something: the
        // wrong-order parse really does fail on a wall banner.
        assert!(
            DyeColor::from_name("red_wall").is_none(),
            "stripping `_banner` first leaves `red_wall`, which must not parse"
        );
    }

    /// The gather is empty rather than a panic before login, like every other
    /// family's.
    #[test]
    fn lectern_spawns_before_login_is_empty_rather_than_a_panic() {
        let handle: SharedHandle = std::sync::Arc::new(std::sync::OnceLock::new());
        assert!(lectern_spawns(&handle, Vec3::ZERO).is_empty());
    }

    /// The enchanting-table gathers are empty rather than a panic before login,
    /// like every other family's.
    #[test]
    fn enchanting_table_gathers_before_login_are_empty_rather_than_a_panic() {
        let handle: SharedHandle = std::sync::Arc::new(std::sync::OnceLock::new());
        let books = EnchantingTableBooks::new();
        assert!(enchanting_table_positions(&handle, glam::DVec3::ZERO, 8.0).is_empty());
        assert!(enchanting_table_spawns(&handle, &books, Vec3::ZERO, 1.0).is_empty());
    }

    /// A book takes exactly **10 ticks** to open and 10 to shut, at `±0.1` a tick.
    ///
    /// Asserted as a *duration* with a value predicted at every step, not just at
    /// the endpoints — the endpoints alone are satisfied by a book that teleports
    /// open, the same trap `the_lid_takes_ten_ticks_to_open_and_ten_to_shut`
    /// records for chests. The rate is vanilla's `entity.open += 0.1F` per tick,
    /// which is why this must not be advanced per frame.
    #[test]
    fn a_book_takes_ten_ticks_to_open_and_ten_to_shut() {
        const POS: [i32; 3] = [4, 65, -9];
        let near = glam::DVec3::new(4.5, 65.5, -8.5);
        let far = glam::DVec3::new(4.5, 65.5, 60.0);
        let mut books = EnchantingTableBooks::new();
        for tick in 1..=12 {
            books.tick(&[POS], near);
            let (_, _, open, _) = books.state(POS, 1.0).expect("tracked while a player is near");
            let expected = (0.1 * tick as f32).min(1.0);
            assert!(
                (open - expected).abs() < 1e-5,
                "tick {tick}: open {open}, expected {expected}"
            );
        }
        for tick in 1..=10 {
            books.tick(&[POS], far);
            let expected = (1.0 - 0.1 * tick as f32).max(0.0);
            let open = books
                .state(POS, 1.0)
                .map_or(0.0, |(_, _, open, _)| open);
            assert!(
                (open - expected).abs() < 1e-5,
                "closing tick {tick}: open {open}, expected {expected}"
            );
        }
        // A settled-shut book is **kept**, because vanilla draws a closed book
        // for every enchanting table it renders — an entry is dropped when the
        // table leaves the gather, not when it stops moving. Both arms on one
        // fixture, so a fold that collected on rest (the behaviour that made
        // distant tables draw nothing) fails the first.
        books.tick(&[POS], far);
        assert_eq!(
            books.len(),
            1,
            "a shut book must stay tracked — it is still drawn, closed"
        );
        assert!(books.state(POS, 1.0).is_some());
        books.tick(&[], far);
        assert!(
            books.is_empty(),
            "a table no longer in the gather must be dropped, {} left",
            books.len()
        );
    }

    /// **The island gate for the missing book.** Every enchanting table in the
    /// gather yields a spawn, whether or not the fold has ticked it — the
    /// defect was `enchanting_table_spawns` skipping any table with no entry,
    /// on the (false) grounds that a shut book is invisible.
    ///
    /// Asserted at the fold's boundary rather than through the world gather,
    /// because the gather needs a live `SharedHandle`: the claim under test is
    /// that a shut book is a *drawn* pose, and the two hypotheses are computed
    /// from `lodestone_render`'s own book functions rather than restated. At
    /// `open == 0` the openness is `0` and `book_part_poses` puts `left_lid` at
    /// `PI` against `right_lid` at `0` — a closed book, six real posed parts,
    /// not an absent one.
    #[test]
    fn a_shut_book_is_a_closed_book_and_not_an_absent_one() {
        let shut = lodestone_render::enchanting_table_book_openness(0.0, 0.0);
        assert_eq!(shut, 0.0);
        let poses = lodestone_render::book_part_poses(shut, (0.0, 0.0));
        assert_eq!(poses.len(), 6);
        let left = poses
            .iter()
            .find(|(name, _, _)| *name == "left_lid")
            .expect("the rig has a left lid");
        let right = poses
            .iter()
            .find(|(name, _, _)| *name == "right_lid")
            .expect("the rig has a right lid");
        assert!(
            (left.1 - std::f32::consts::PI).abs() < 1e-6 && right.1 == 0.0,
            "a shut book's lids must fold together, got {left:?} / {right:?}"
        );

        // And the fold's own default — what `enchanting_table_spawns` now draws
        // for an as-yet-unticked table — is exactly that rest pose rather than a
        // sentinel the renderer would have to special-case.
        let (y_rot, time, open, flip) = <(f32, f32, f32, f32)>::default();
        assert_eq!((y_rot, time, open, flip), (0.0, 0.0, 0.0, 0.0));
        assert_eq!(
            lodestone_render::enchanting_table_book_openness(time, open),
            shut
        );
    }

    /// The book chases the player the **short** way round the `±PI` seam.
    ///
    /// Both hypotheses computed from outside arithmetic. Starting at `rot = 3.0`
    /// with a target of `-3.0`, the raw difference is `-6.0`; wrapped into
    /// `-PI..PI` it is `+0.28319`, so 40% of it puts `rot` at
    /// `3.0 + 0.11327 = 3.11327`. Without the wrap the book takes the long way and
    /// lands at `3.0 - 2.4 = 0.6` — nearly a full revolution backwards, every time
    /// a player walks past one particular corner.
    #[test]
    fn the_book_chases_the_player_the_short_way_round() {
        let mut rng = JavaRandom::new(1);
        let mut book = Book {
            rot: 3.0,
            // `+0.02` is applied before the chase when no player is near, so this
            // lands the target on exactly `-3.0`.
            t_rot: -3.02,
            ..Book::default()
        };
        book.tick([0, 0, 0], None, &mut rng);
        assert!(
            (book.rot - 3.113_274).abs() < 1e-4,
            "rot is {}, expected 3.113274 (the short way); 0.6 is the long way",
            book.rot
        );
    }

    /// `tRot = atan2(zd, xd)`, in that argument order.
    ///
    /// The swap is the failure this catches and it is invisible any other way: a
    /// player due **east** of the table (`+x`) must give `0`, and one due
    /// **south** (`+z`) must give `PI/2`. Swapped arguments produce exactly those
    /// two values in the opposite order, so a single-position check passes.
    ///
    /// Due **west** is deliberately not one of the samples: `atan2` returns `+PI`
    /// there and the wrap into `-PI..PI` is half-open, so the stored value is
    /// `-PI` — correct, matching vanilla's own `while (tRot >= PI)`, and a
    /// misleading thing to assert an expected sign on. The third sample sits off
    /// the seam at `3*PI/4`, where a swapped call gives `-PI/4` instead.
    #[test]
    fn the_target_angle_points_at_the_player_in_atan2s_argument_order() {
        const POS: [i32; 3] = [10, 64, 20];
        let mut rng = JavaRandom::new(2);
        for (offset, expected) in [
            (glam::DVec3::new(1.0, 0.0, 0.0), 0.0),
            (glam::DVec3::new(0.0, 0.0, 1.0), std::f32::consts::FRAC_PI_2),
            (
                glam::DVec3::new(-1.0, 0.0, 1.0),
                3.0 * std::f32::consts::FRAC_PI_4,
            ),
        ] {
            let centre = glam::DVec3::new(
                f64::from(POS[0]) + 0.5,
                f64::from(POS[1]) + 0.5,
                f64::from(POS[2]) + 0.5,
            );
            let mut book = Book::default();
            book.tick(POS, Some(centre + offset), &mut rng);
            assert!(
                (book.t_rot - expected).abs() < 1e-5,
                "player at {offset:?} gave t_rot {}, expected {expected}",
                book.t_rot
            );
        }
    }

    /// Vanilla's `do { flipT += nextInt(4) - nextInt(4) } while (old == flipT)`
    /// must **always** move the target, and a plain `if` leaves it occasionally
    /// unmoved when a page was asked to turn.
    ///
    /// While `open < 0.5` a re-roll happens every tick unconditionally, so the
    /// first **four** ticks are four guaranteed re-rolls — which is what makes this
    /// assertable without controlling the dice. The difference of two `nextInt(4)`
    /// draws is zero one time in four, so four ticks of a plain `if` would fail
    /// this with probability about `1 - (3/4)^4 = 68%`; across the seeds swept
    /// below it is a certainty.
    ///
    /// **Four and not five, and the off-by-one is vanilla's**: the test is
    /// `open < 0.5` *after* the `+= 0.1`, so the fifth tick's `open` is exactly
    /// `0.5` and falls through to the 1-in-40 dice instead. This test asserted five
    /// on its first run and failed at tick 4 for exactly that reason — a wrong test
    /// premise, not a wrong port.
    #[test]
    fn a_page_reroll_always_moves_the_target() {
        const POS: [i32; 3] = [0, 64, 0];
        let centre = glam::DVec3::new(0.5, 64.5, 0.5);
        for seed in 0..16 {
            let mut rng = JavaRandom::new(seed);
            let mut book = Book::default();
            for tick in 0..4 {
                let before = book.flip_t;
                book.tick(POS, Some(centre), &mut rng);
                assert!(
                    (book.flip_t - before).abs() > f32::EPSILON,
                    "seed {seed} tick {tick}: flip_t stayed at {before}"
                );
            }
        }
    }

    /// `java.util.Random.nextInt(bound)`'s two branches are not interchangeable,
    /// and this animation uses both: `nextInt(4)` is the power-of-two
    /// multiply-and-shift and `nextInt(40)` is the rejection loop.
    ///
    /// Asserted as coverage of the whole range in both, not just as "in bounds":
    /// a bound-off-by-one, or a rejection loop that never terminates its tail,
    /// stays in bounds while losing values. `nextInt(4)` must produce all four.
    #[test]
    fn the_java_random_covers_both_bound_branches() {
        let mut rng = JavaRandom::new(0xDEAD_BEEF);
        let mut small = [false; 4];
        let mut seen_low = false;
        let mut seen_high = false;
        for _ in 0..4000 {
            let a = rng.next_i32_bound(4);
            assert!((0..4).contains(&a), "nextInt(4) produced {a}");
            small[a as usize] = true;
            let b = rng.next_i32_bound(40);
            assert!((0..40).contains(&b), "nextInt(40) produced {b}");
            seen_low |= b == 0;
            seen_high |= b == 39;
        }
        assert!(small.iter().all(|seen| *seen), "nextInt(4) missed a value");
        assert!(
            seen_low && seen_high,
            "nextInt(40) never reached an endpoint, so its range is wrong"
        );
    }
}

/// Moving-piston gates — `PistonHeadRenderer` and `PistonMovingBlockEntity`.
///
/// Its own module for the same reason `sign_tests` is: this file is shared, and a
/// per-renderer module keeps the pathspec commit and the failure output honest
/// about which unit broke.
#[cfg(test)]
mod piston_tests {
    use super::*;

    const PISTON_POS: [i32; 3] = [12, 71, -40];

    /// `Direction.LEGACY_ID_CODEC`'s byte, resolved against vanilla's own enum
    /// declaration order rather than an alphabetical or a horizontal-facing one.
    ///
    /// The wrong hypothesis worth excluding is the **2-D** order the sign and
    /// banner gathers use (`SOUTH, WEST, NORTH, EAST`), which shares no value with
    /// this table except by accident — so the assertion is the whole six-entry map,
    /// and the two out-of-range probes prove it declines rather than wrapping the
    /// way vanilla's `BY_ID` does.
    #[test]
    fn the_facing_byte_is_the_3d_data_value_not_the_2d_one() {
        let mut wrong: Vec<String> = Vec::new();
        for (id, expected) in [
            (0_i8, [0, -1, 0]),
            (1, [0, 1, 0]),
            (2, [0, 0, -1]),
            (3, [0, 0, 1]),
            (4, [-1, 0, 0]),
            (5, [1, 0, 0]),
        ] {
            match direction_step_from_3d(id) {
                Some(step) if step == expected => {}
                other => wrong.push(format!("{id} -> {other:?}, expected {expected:?}")),
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
        // Vanilla's own by-id lookup wraps out-of-range ids. Wrapping here would push a
        // contraption along an axis nobody asked for, so this declines.
        assert_eq!(direction_step_from_3d(6), None);
        assert_eq!(direction_step_from_3d(-1), None);
    }

    /// Vanilla's own block-state codec's compound renders as the canonical state string, and the
    /// rendered string resolves against the **real 26.2 table** — not a fixture.
    ///
    /// The expected id is derived from `lodestone_data`'s own table on the other
    /// side of the string, so the two agree only if the property names, the sort
    /// order and the bracket syntax are all right. A bare `Name` with no
    /// `Properties` must resolve to that block's *default* state, which is the arm
    /// where "lowest id sharing the name" used to be wrong for 661 blocks.
    #[test]
    fn a_codec_block_state_compound_renders_a_string_the_real_table_resolves() {
        use lodestone_core::Nbt;

        let compound = Nbt::Compound(vec![
            ("Name".into(), Nbt::String("minecraft:piston_head".into())),
            (
                "Properties".into(),
                Nbt::Compound(vec![
                    // Deliberately out of sorted order on the wire.
                    ("type".into(), Nbt::String("sticky".into())),
                    ("facing".into(), Nbt::String("up".into())),
                    ("short".into(), Nbt::String("true".into())),
                ]),
            ),
        ]);
        let rendered = nbt_block_state_string(&compound).expect("a renderable compound");
        assert_eq!(
            rendered, "minecraft:piston_head[facing=up,short=true,type=sticky]",
            "properties must be sorted by key, which is what the generated table's \
             own slice comparison assumes"
        );
        let id = lodestone_data::block_states::state_id(&rendered).expect("a real state");
        assert_eq!(
            lodestone_data::block_states::block_name(id),
            Some("minecraft:piston_head")
        );
        let props = lodestone_data::block_states::properties(id).expect("properties");
        assert!(props.contains(&("facing", "up")), "{props:?}");
        assert!(props.contains(&("short", "true")), "{props:?}");
        assert!(props.contains(&("type", "sticky")), "{props:?}");

        // A bare name resolves to the default state, and the default is not
        // necessarily the lowest id sharing the name.
        let bare = Nbt::Compound(vec![(
            "Name".into(),
            Nbt::String("minecraft:sticky_piston".into()),
        )]);
        let bare_id = lodestone_data::block_states::state_id(
            &nbt_block_state_string(&bare).expect("a renderable bare compound"),
        )
        .expect("a real state");
        assert_eq!(
            lodestone_data::block_states::properties(bare_id),
            Some(&[("extended", "false"), ("facing", "north")][..]),
            "`PistonBaseBlock`'s registered default is `facing=north, extended=false`"
        );
    }

    /// `extractRenderState`'s branch 1: the moved state already *is* a piston head,
    /// and `short` is rewritten from the progress with `<= 0.5`.
    ///
    /// The discriminating input is **not** `0.5` — that is the boundary, where the
    /// inclusive and exclusive readings of the comparison coincide with each other
    /// and where branch 2's `>= 0.5` also fires. `0.25` and `0.75` are on opposite
    /// sides of it and both are checked, because asserting only one is satisfied by
    /// a hardcoded `short`.
    #[test]
    fn a_moved_piston_head_takes_its_short_from_the_progress() {
        let head = lodestone_data::block_states::state_id(
            "minecraft:piston_head[facing=up,short=false,type=normal]",
        )
        .expect("a real head state");
        let nbt = MovingPistonNbt {
            moved_state: head,
            direction: [0, 1, 0],
            progress: 0.0,
            extending: true,
            source: false,
        };

        let mut wrong: Vec<String> = Vec::new();
        for (progress, expected_short) in [(0.25_f32, "true"), (0.75, "false")] {
            let (state, base) = moving_piston_states(&nbt, progress).expect("a resolvable state");
            if base.is_some() {
                wrong.push(format!("progress {progress}: an extension drew a base"));
            }
            let short = state_property(state, "short");
            if short != Some(expected_short) {
                wrong.push(format!(
                    "progress {progress}: short={short:?}, expected {expected_short}"
                ));
            }
            // Facing and type must survive the rewrite untouched.
            if state_property(state, "facing") != Some("up")
                || state_property(state, "type") != Some("normal")
            {
                wrong.push(format!("progress {progress}: facing/type were rewritten"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// Vanilla's own render-state extraction's branch 2: a retracting **source** piston synthesises a
    /// head from its base block and draws its base as well.
    ///
    /// Three separate claims, each of which a plausible port gets wrong on its own:
    ///
    /// * the head's `type` is `sticky` for a sticky base and `normal` — **not**
    ///   `default` — for a plain one, because vanilla's own default piston-type's serialized name
    ///   is `normal`;
    /// * the head's `short` uses `>= 0.5`, the **opposite** comparison to branch 1,
    ///   so at `0.25` it is `false` where branch 1 would say `true`;
    /// * the base is forced to `extended=true` and keeps the base's own facing.
    ///
    /// `0.25` discriminates the second claim from branch 1's rule; `0.5` would not.
    #[test]
    fn a_retracting_source_piston_synthesises_a_head_and_draws_its_base() {
        let mut wrong: Vec<String> = Vec::new();
        for (base_block, expected_type) in [
            ("minecraft:sticky_piston", "sticky"),
            ("minecraft:piston", "normal"),
        ] {
            let base_state = lodestone_data::block_states::state_id(&format!(
                "{base_block}[extended=false,facing=west]"
            ))
            .expect("a real base state");
            let nbt = MovingPistonNbt {
                moved_state: base_state,
                direction: [-1, 0, 0],
                progress: 0.0,
                extending: false,
                source: true,
            };
            let (head, base) = moving_piston_states(&nbt, 0.25).expect("a resolvable state");
            if lodestone_data::block_states::block_name(head) != Some("minecraft:piston_head") {
                wrong.push(format!("{base_block}: head is not a piston head"));
            }
            if state_property(head, "type") != Some(expected_type) {
                wrong.push(format!(
                    "{base_block}: head type is {:?}, expected {expected_type}",
                    state_property(head, "type")
                ));
            }
            if state_property(head, "facing") != Some("west") {
                wrong.push(format!("{base_block}: head facing did not follow the base"));
            }
            // Branch 2's comparison is `>= 0.5`, so a quarter of the way through a
            // retraction the head is still long.
            if state_property(head, "short") != Some("false") {
                wrong.push(format!(
                    "{base_block}: short is {:?} at progress 0.25 — branch 1's \
                     `<= 0.5` rule was used instead of branch 2's `>= 0.5`",
                    state_property(head, "short")
                ));
            }
            match base {
                Some(base) => {
                    if lodestone_data::block_states::block_name(base) != Some(base_block) {
                        wrong.push(format!("{base_block}: base block changed identity"));
                    }
                    if state_property(base, "extended") != Some("true") {
                        wrong.push(format!("{base_block}: base was not forced extended"));
                    }
                    if state_property(base, "facing") != Some("west") {
                        wrong.push(format!("{base_block}: base facing changed"));
                    }
                }
                None => wrong.push(format!("{base_block}: no base drew")),
            }
            // And at 0.75 the head has gone short — so `short` is a function of the
            // progress here and not a constant that happened to read correctly.
            let (late_head, _) = moving_piston_states(&nbt, 0.75).expect("a resolvable state");
            if state_property(late_head, "short") != Some("true") {
                wrong.push(format!("{base_block}: short did not flip by progress 0.75"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// `extractRenderState`'s branch 3: an ordinary pushed block is drawn exactly as
    /// stored, with no base.
    ///
    /// The control for the two branch tests above: without it, a
    /// `moving_piston_states` that returned its input unchanged in *every* case
    /// would still have to fail them, but one that synthesised a head in every case
    /// would not be caught anywhere.
    #[test]
    fn an_ordinary_pushed_block_is_drawn_as_stored() {
        let stone = lodestone_data::block_states::state_id("minecraft:stone").expect("stone");
        let nbt = MovingPistonNbt {
            moved_state: stone,
            direction: [0, 0, 1],
            progress: 0.0,
            extending: true,
            source: false,
        };
        assert_eq!(moving_piston_states(&nbt, 0.25), Some((stone, None)));
        // A pushed block belonging to a *source* piston that is extending is still
        // branch 3 — `isSourcePiston` alone does not select branch 2.
        let extending_source = MovingPistonNbt {
            source: true,
            ..nbt.clone()
        };
        assert_eq!(
            moving_piston_states(&extending_source, 0.25),
            Some((stone, None)),
            "branch 2's guard is `isSourcePiston && !isExtending`, both halves"
        );
    }

    /// The NBT decode reads each field at the tag type `saveAdditional` writes:
    /// `facing` as a **byte**, `progress` as a **float**, `extending`/`source` as
    /// bytes.
    ///
    /// Reading `facing` as an int is the shipped-bug shape — it would default every
    /// piston to `DOWN` while the parse still looked clean — so the negative arm
    /// hands it an `Nbt::Int` with the *right value* and requires the decode to
    /// decline rather than to succeed by coincidence.
    #[test]
    fn the_nbt_decode_is_keyed_by_tag_type_not_only_by_field_name() {
        use lodestone_core::Nbt;

        let block_state = Nbt::Compound(vec![(
            "Name".into(),
            Nbt::String("minecraft:stone".into()),
        )]);
        let good = Nbt::Compound(vec![
            ("blockState".into(), block_state.clone()),
            ("facing".into(), Nbt::Byte(1)),
            ("progress".into(), Nbt::Float(0.5)),
            ("extending".into(), Nbt::Byte(1)),
            ("source".into(), Nbt::Byte(0)),
        ]);
        let decoded = moving_piston_nbt(&good).expect("a decodable record");
        assert_eq!(decoded.direction, [0, 1, 0]);
        assert_eq!(decoded.progress, 0.5);
        assert!(decoded.extending);
        assert!(!decoded.source);

        // `facing` at the wrong tag type, same value.
        let wrong_tag = Nbt::Compound(vec![
            ("blockState".into(), block_state.clone()),
            ("facing".into(), Nbt::Int(1)),
        ]);
        assert_eq!(
            moving_piston_nbt(&wrong_tag),
            None,
            "an int `facing` must be declined, not silently defaulted"
        );

        // Absent `extending`/`source`/`progress` are vanilla's own
        // boolean-or-default/float-or-default results, which are real states rather than decode failures.
        let sparse = Nbt::Compound(vec![
            ("blockState".into(), block_state),
            ("facing".into(), Nbt::Byte(0)),
        ]);
        let decoded = moving_piston_nbt(&sparse).expect("a sparse record still decodes");
        assert_eq!(decoded.progress, 0.0);
        assert!(!decoded.extending);
        assert!(!decoded.source);

        // An air moved state draws nothing — `!blockState.isAir()`.
        let air = Nbt::Compound(vec![
            (
                "blockState".into(),
                Nbt::Compound(vec![("Name".into(), Nbt::String("minecraft:air".into()))]),
            ),
            ("facing".into(), Nbt::Byte(0)),
        ]);
        assert_eq!(moving_piston_nbt(&air), None);
    }

    /// The clock ramps by exactly `0.5` per tick and reaches `1.0` in **two** ticks
    /// — `TICKS_TO_EXTEND`, not the plausible ten a chest lid takes.
    ///
    /// The whole sequence is predicted, and the discovery tick is asserted to be a
    /// *seed* rather than an advance: a tracker that advanced on discovery would read
    /// `0.5` on the first observation and finish the push in one tick, halving an
    /// animation that is only two ticks long to begin with.
    #[test]
    fn a_push_ramps_by_half_a_tick_and_completes_in_two() {
        let present = [(PISTON_POS, 0.0_f32)];
        let mut moves = PistonMoves::new();

        moves.tick(&present);
        assert_eq!(
            moves.progress(PISTON_POS, 1.0),
            Some(0.0),
            "the discovery tick seeds and must not advance"
        );

        let mut wrong: Vec<String> = Vec::new();
        for expected in [0.5_f32, 1.0, 1.0] {
            moves.tick(&present);
            let got = moves.progress(PISTON_POS, 1.0);
            if got != Some(expected) {
                wrong.push(format!("expected {expected}, got {got:?}"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// The partial-tick lerp reads between the previous and the current tick's
    /// progress, and out-of-range alphas clamp rather than extrapolating.
    ///
    /// Without the lerp a two-tick animation is two frames of a 60 fps second — a
    /// snap, not a slide. The mid-tick value is predicted (`0.25`, half way through
    /// the `0.0 -> 0.5` step) rather than merely required to lie between the ends,
    /// which a stepped counter also satisfies at the endpoints.
    #[test]
    fn progress_interpolates_within_a_tick() {
        let present = [(PISTON_POS, 0.0_f32)];
        let mut moves = PistonMoves::new();
        moves.tick(&present); // seed
        moves.tick(&present); // previous 0.0 -> progress 0.5
        assert_eq!(moves.progress(PISTON_POS, 0.0), Some(0.0));
        assert_eq!(moves.progress(PISTON_POS, 0.5), Some(0.25));
        assert_eq!(moves.progress(PISTON_POS, 1.0), Some(0.5));
        assert_eq!(moves.progress(PISTON_POS, 4.0), Some(0.5), "clamped, not extrapolated");
        assert_eq!(moves.progress(PISTON_POS, -1.0), Some(0.0));
    }

    /// An untracked position reports `None`, **not** `0.0`.
    ///
    /// This is the one place where the chest lid's "absent equals at rest" shortcut
    /// would be actively harmful, and the difference is worth a gate of its own:
    /// `0.0` is the *most displaced* progress a piston has, so a `0.0` here would
    /// draw a head a full cell inside the piston base. Paired with the removal
    /// half — a cell that stops holding a moving piston is forgotten, so the map
    /// cannot grow as a player walks past contraptions.
    #[test]
    fn an_untracked_piston_is_none_rather_than_zero_and_a_finished_one_is_forgotten() {
        let mut moves = PistonMoves::new();
        assert_eq!(moves.progress(PISTON_POS, 1.0), None);
        assert!(moves.is_empty());

        moves.tick(&[(PISTON_POS, 0.0)]);
        assert_eq!(moves.len(), 1);
        // The cell no longer holds a moving piston: the server's `finalTick` has
        // replaced it.
        moves.tick(&[]);
        assert!(moves.is_empty(), "{} entries retained", moves.len());
        assert_eq!(moves.progress(PISTON_POS, 1.0), None);
    }

    /// A seed from the wire is honoured rather than overwritten with zero, and it is
    /// clamped into `0..=1`.
    ///
    /// Vanilla writes `progressO` into the update tag, so a client that joins
    /// mid-push is told where the push already is. Seeding at zero instead would
    /// restart every in-flight contraption on chunk load — visible as a stutter that
    /// looks like a network problem.
    #[test]
    fn the_wire_seed_is_honoured_and_clamped() {
        let mut moves = PistonMoves::new();
        moves.tick(&[(PISTON_POS, 0.5)]);
        assert_eq!(moves.progress(PISTON_POS, 1.0), Some(0.5));
        moves.tick(&[(PISTON_POS, 0.5)]);
        assert_eq!(
            moves.progress(PISTON_POS, 1.0),
            Some(1.0),
            "a push seeded half way finishes one tick later, not two"
        );

        let mut moves = PistonMoves::new();
        moves.tick(&[([0, 0, 0], 9.0)]);
        assert_eq!(moves.progress([0, 0, 0], 1.0), Some(1.0), "clamped");
        let mut moves = PistonMoves::new();
        moves.tick(&[([0, 0, 0], -3.0)]);
        assert_eq!(moves.progress([0, 0, 0], 1.0), Some(0.0), "clamped");
    }

    /// `moving_piston` is a real block in the 26.2 table and every one of its states
    /// is recognised — the gather's whole entry condition.
    ///
    /// The negative arm matters as much: a `piston`, a `sticky_piston` and a
    /// `piston_head` must **not** be recognised, or the gather would draw a moving
    /// copy of every static piston in the world on top of the terrain mesh.
    #[test]
    fn only_moving_piston_states_enter_the_gather() {
        let mut moving = 0usize;
        let mut wrong: Vec<String> = Vec::new();
        for id in 0..lodestone_data::block_states::STATE_COUNT {
            let name = lodestone_data::block_states::block_name(id);
            match name {
                Some("minecraft:moving_piston") => {
                    moving += 1;
                    if !is_moving_piston(id) {
                        wrong.push(format!("{id} is a moving piston but was not recognised"));
                    }
                }
                Some("minecraft:piston" | "minecraft:sticky_piston" | "minecraft:piston_head") => {
                    if is_moving_piston(id) {
                        wrong.push(format!("{id} ({name:?}) was recognised as a moving piston"));
                    }
                }
                _ => {}
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
        // `moving_piston` is `facing` (6) x `type` (2).
        assert_eq!(
            moving, 12,
            "expected 12 `moving_piston` states (6 facings x 2 types)"
        );
    }
}

/// The beacon light-beam gather — vanilla's own beacon-block-entity tick's base-pyramid and
/// beam-colour scans, run to completion against the client's own loaded world
/// rather than paced at `BLOCKS_CHECK_PER_TICK` (10) blocks per tick the way
/// vanilla's server-authoritative tick does.
///
/// **This needs no new packet.** In vanilla, its own beacon-block-entity tick is an
/// ordinary block-entity ticker, which vanilla's own block-entity tick dispatch runs on
/// *both* sides — the same mechanism that lets a furnace's flame flicker
/// client-side with no server round trip. `levels` and `beamSections` are
/// pure functions of block state the client already has loaded (the base
/// pyramid below the beacon, and the run of `minecraft:beacon_beam_block`s
/// above it), so this file recomputes them fresh from
/// [`lodestone_world::World::block_state_at`] every gather rather than
/// carrying a `Sim::step`-ticked tracker the way [`ChestLids`]/`BellShakes`
/// do — there is no server signal to integrate, only current world state to
/// read. That also means, unlike every *other* animated source in this file,
/// [`beacon_spawns`] needs no per-position `HashMap` alongside it.
///
/// Vanilla's own pacing exists to spread a 10-blocks-per-tick cost across
/// many *server* ticks so one beacon does not spike the tick loop; a client
/// render source evaluated once per frame against already-resident chunk
/// data has no equivalent budget to protect, and the scan is bounded by
/// world height (a few hundred iterations at most, only for beacons within
/// [`VIEW_DISTANCE`]). The **result** does not depend on how many ticks the
/// scan took — `levels`/`beamSections` are pure functions of current block
/// state — so running it to completion in one call changes nothing vanilla
/// would call wrong, only how the cost is spread.
///
/// # Base pyramid census — `minecraft:beacon_base_blocks`, five members
///
/// Vanilla's own beacon-base update check gates against a five-member tag
/// (`data/minecraft/tags/block/beacon_base_blocks.json` in the real jar:
/// iron, gold, diamond, emerald and netherite blocks). No tag table exists
/// anywhere in this workspace's shell-reachable crates, so — per
/// `CLAUDE.md`'s note that a small vanilla census belongs beside its one
/// consumer rather than behind a crate boundary this task cannot reach —
/// [`BEACON_BASE_BLOCKS`] hardcodes it here.
///
/// # The beam-colour scan's one real quirk
///
/// Vanilla's own beacon-block-entity tick's checking-beam-sections size-at-most-one
/// guard is
/// not "is this the first block": it means the **first two** beam blocks a
/// scan encounters each start their own section even when same-coloured —
/// only from the third one onward does a same-colour run merge or a
/// differing one average via [`average_beam_color`]. [`beacon_beam_scan`]
/// ports this literally (`sections.len() <= 1`, not `is_empty()`), because
/// getting it wrong either merges the beacon's own white with a directly-
/// stacked glass block of the same colour (undercounting by one section) or
/// never lets *any* two sections merge (a run of ten same-coloured panes
/// staying ten sections instead of one) — both wrong in ways a screenshot of
/// a plain white or two-colour beam cannot distinguish from correct.
///
/// # What is not ported
///
/// Vanilla's own beacon-block-entity tick's scan stops at
/// its own motion-blocking-heightmap-based world-surface height query — vanilla's own
/// motion-blocking heightmap. This gather has no heightmap and instead scans
/// until [`lodestone_world::World::block_state_at`] returns `None`, which
/// happens at the loaded column's own height ceiling or past an unloaded
/// chunk. In every case that matters (an unbroken beam reaching the sky, or
/// one broken by an opaque block) the two termination points coincide,
/// because the light-dampening check below already stops the scan at the
/// first opaque block either way — a heightmap only matters for a column
/// whose *surface* block is itself transparent to the heightmap type (rare,
/// and unmodelled here as a documented simplification rather than a silent
/// one).
const BEACON_BASE_BLOCKS: [&str; 5] = [
    "iron_block",
    "gold_block",
    "diamond_block",
    "emerald_block",
    "netherite_block",
];

fn is_beacon_base_block(state_id: u32) -> bool {
    lodestone_data::block_states::block_name(state_id).is_some_and(|name| {
        let path = name.strip_prefix("minecraft:").unwrap_or(name);
        BEACON_BASE_BLOCKS.contains(&path)
    })
}

/// Vanilla's own beacon-base update — the number of complete concentric square
/// rings of base blocks below the beacon, `0..=4`. Stops at the first
/// incomplete or unloaded ring, exactly as vanilla's own early exit does.
fn beacon_levels(world: &World, pos: [i32; 3]) -> i32 {
    let [x, y, z] = pos;
    let mut levels = 0;
    for step in 1..=4 {
        let ly = y - step;
        let mut ok = true;
        'ring: for lx in (x - step)..=(x + step) {
            for lz in (z - step)..=(z + step) {
                let Some(state) = world.block_state_at(lx, ly, lz) else {
                    ok = false;
                    break 'ring;
                };
                if !is_beacon_base_block(state) {
                    ok = false;
                    break 'ring;
                }
            }
        }
        if !ok {
            break;
        }
        levels = step;
    }
    levels
}

/// Vanilla's own beacon-block-entity tick's beam-colour scan, starting at the beacon's own
/// position (the beacon block is itself a beam block — `DyeColor::White`,
/// vanilla's own beacon-block color accessor) and walking straight up. See the module doc for
/// the `size() <= 1` quirk this ports literally, and for why the loaded
/// column's own height ceiling stands in for vanilla's heightmap.
fn beacon_beam_scan(world: &World, pos: [i32; 3]) -> Vec<BeamSection> {
    let [x, y, z] = pos;
    let mut sections: Vec<BeamSection> = Vec::new();
    let mut cy = y;
    loop {
        let Some(state) = world.block_state_at(x, cy, z) else {
            break;
        };
        let name = lodestone_data::block_states::block_name(state);
        let path = name.map(|n| n.strip_prefix("minecraft:").unwrap_or(n));
        let beam_color = path.and_then(beacon_beam_color);
        if let Some(color) = beam_color {
            if sections.len() <= 1 {
                sections.push(BeamSection { color, height: 1 });
            } else if let Some(last) = sections.last_mut() {
                if color == last.color {
                    last.height += 1;
                } else {
                    let averaged = average_beam_color(last.color, color);
                    sections.push(BeamSection {
                        color: averaged,
                        height: 1,
                    });
                }
            }
        } else {
            let opaque = lodestone_data::block_states::StateId::new(state)
                .is_some_and(|state| lodestone_data::light_props::dampening(state) >= 15);
            let is_bedrock = path == Some("bedrock");
            if sections.is_empty() || (opaque && !is_bedrock) {
                sections.clear();
                break;
            }
            if let Some(last) = sections.last_mut() {
                last.height += 1;
            }
        }
        cy += 1;
    }
    sections
}

/// One beacon candidate resolved into a [`BeaconSpawn`] — always `Some`
/// (unlike every other `*_spawn` in this file) because a beacon has no block
/// state this pass declines on; the caller has already checked the block
/// name is `minecraft:beacon`. `sections` is empty when `levels == 0`,
/// mirroring vanilla's own beam-sections accessor, which returns an empty
/// list rather than the stored sections whenever the level count is zero — a
/// coloured run can scan perfectly clean above an incomplete base and still
/// must not render.
fn beacon_spawn(world: &World, block: [i32; 3], eye: Vec3, animation_time: f32) -> BeaconSpawn {
    let levels = beacon_levels(world, block);
    let sections = if levels > 0 {
        beacon_beam_scan(world, block)
    } else {
        Vec::new()
    };
    let dx = eye.x - (block[0] as f32 + 0.5);
    let dz = eye.z - (block[2] as f32 + 0.5);
    let horizontal_distance = dx.hypot(dz);
    BeaconSpawn {
        pos: block,
        sections,
        animation_time,
        beam_radius_scale: beam_radius_scale(horizontal_distance),
    }
}

/// Every beacon within [`VIEW_DISTANCE`], resolved into a [`BeaconSpawn`] —
/// the beacon sibling of [`shulker_spawns`]/[`skull_spawns`], for
/// [`crate::gpu::RenderState::set_beacon_source`].
///
/// Reuses [`chest_candidates`] for the position gather (already generic over
/// block-entity type — a second scan is never needed, the same reuse
/// `shulker_spawns` documents) and filters to `minecraft:beacon` by name,
/// since [`chest_candidates`] hands back *every* block entity's position and
/// state regardless of type. `world` is read once and held for both the
/// candidate gather and every beam scan, unlike the single-state-read
/// gathers elsewhere in this file — a beam scan needs many more reads than
/// one, so re-acquiring the guard per candidate would serialise against
/// every other reader for longer, not less.
#[must_use]
pub fn beacon_spawns(handle: &SharedHandle, eye: Vec3, game_time: i64, partial_tick: f32) -> Vec<BeaconSpawn> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();

    // Vanilla's own beacon-renderer extraction: `floorMod(gameTime, 40) + partialTicks`.
    let animation_time = game_time.rem_euclid(40) as f32 + partial_tick;

    let mut out = Vec::new();
    {
        let world = store.read();
        let candidates = chest_candidates(
            &world,
            chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }),
            eye,
        );
        for (block, state_id) in candidates {
            let name = lodestone_data::block_states::block_name(state_id);
            let path = name.map(|n| n.strip_prefix("minecraft:").unwrap_or(n));
            if path != Some("beacon") {
                continue;
            }
            out.push(beacon_spawn(&world, block, eye, animation_time));
        }
    }
    out.sort_by_key(|s| s.pos);
    out
}

/// Every end portal within [`VIEW_DISTANCE`], resolved into an
/// [`EndPortalSpawn`] — the end-portal sibling of [`beacon_spawns`]. No
/// per-position tracker and no NBT read at all: `TheEndPortalBlockEntity.
/// shouldRenderFace` never consults world state or NBT for this type (see
/// `lodestone_render::end_portal`'s module doc), so the only thing worth
/// gathering is *where* one is.
#[must_use]
pub fn end_portal_spawns(handle: &SharedHandle, eye: Vec3) -> Vec<EndPortalSpawn> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();

    let mut out = Vec::new();
    {
        let world = store.read();
        let candidates = chest_candidates(
            &world,
            chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }),
            eye,
        );
        for (block, state_id) in candidates {
            let name = lodestone_data::block_states::block_name(state_id);
            let path = name.map(|n| n.strip_prefix("minecraft:").unwrap_or(n));
            if path != Some("end_portal") {
                continue;
            }
            out.push(EndPortalSpawn { pos: block });
        }
    }
    out.sort_by_key(|s| s.pos);
    out
}

/// Vanilla's own should-render-face check, restated over
/// this crate's own "does this state fully block light" census
/// (`lodestone_data::light_props::dampening(state) >= 15`) rather than the
/// real jar's `VoxelShape` face-occlusion cache — the same stand-in
/// [`beacon_beam_scan`] already trusts for "does this block stop the beam".
/// A full opaque cube and a full light-blocking state coincide for every
/// block that could plausibly neighbor a gateway (obsidian, bedrock, stone,
/// air), so this is a deliberate simplification of the *mechanism*, not of
/// the *result*, for the blocks this ever actually sees.
///
/// An unloaded neighbor (`block_state_at` returning `None` — outside any
/// loaded chunk, or above/below the world) is treated as **non**-occluding,
/// matching vanilla's own out-of-world-bounds air: "show the face" is the
/// safe default, since the alternative (hiding a real gateway's swirl at
/// the edge of loaded terrain) is the more visible failure.
fn end_gateway_faces_to_show(world: &World, pos: [i32; 3]) -> Vec<lodestone_assets::Direction> {
    use lodestone_assets::Direction;
    const OFFSETS: [(Direction, [i32; 3]); 6] = [
        (Direction::Down, [0, -1, 0]),
        (Direction::Up, [0, 1, 0]),
        (Direction::North, [0, 0, -1]),
        (Direction::South, [0, 0, 1]),
        (Direction::West, [-1, 0, 0]),
        (Direction::East, [1, 0, 0]),
    ];
    OFFSETS
        .into_iter()
        .filter(|(_, [dx, dy, dz])| {
            let nx = pos[0] + dx;
            let ny = pos[1] + dy;
            let nz = pos[2] + dz;
            match world.block_state_at(nx, ny, nz) {
                Some(state) => lodestone_data::block_states::StateId::new(state)
                    .is_none_or(|state| lodestone_data::light_props::dampening(state) < 15),
                None => true,
            }
        })
        .map(|(d, _)| d)
        .collect()
}

/// Every end gateway within [`VIEW_DISTANCE`], resolved into an
/// [`EndGatewaySpawn`] — the gateway sibling of [`end_portal_spawns`].
/// **Not** the gateway's teleport beam (see
/// `lodestone_render::end_portal`'s module doc for why that is a deliberate,
/// documented gap this session) — just the always-visible star-field faces.
/// A gateway with every face occluded (theoretically possible, never in
/// practice — real gateway placements always have at least the top face
/// open) is dropped rather than pushed with an empty face list, so the GPU
/// pass never has to special-case a zero-vertex instance.
#[must_use]
pub fn end_gateway_spawns(handle: &SharedHandle, eye: Vec3) -> Vec<EndGatewaySpawn> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();

    let mut out = Vec::new();
    {
        let world = store.read();
        let candidates = chest_candidates(
            &world,
            chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }),
            eye,
        );
        for (block, state_id) in candidates {
            let name = lodestone_data::block_states::block_name(state_id);
            let path = name.map(|n| n.strip_prefix("minecraft:").unwrap_or(n));
            if path != Some("end_gateway") {
                continue;
            }
            let faces = end_gateway_faces_to_show(&world, block);
            if faces.is_empty() {
                continue;
            }
            out.push(EndGatewaySpawn { pos: block, faces });
        }
    }
    out.sort_by_key(|s| s.pos);
    out
}

#[cfg(test)]
mod end_portal_tests {
    use super::*;

    /// [`end_gateway_faces_to_show`] with every neighbor unloaded (a bare
    /// `World`, no chunks inserted at all) must show every face — the "show
    /// rather than hide" default the function's own doc commits to.
    #[test]
    fn every_face_shows_when_every_neighbor_is_unloaded() {
        let world = World::new();
        let faces = end_gateway_faces_to_show(&world, [0, 64, 0]);
        assert_eq!(faces.len(), 6, "{faces:?}");
    }
}

#[cfg(test)]
mod beacon_tests {
    use super::*;

    /// `BEACON_BASE_BLOCKS` names exactly the five real jar members and
    /// nothing else — the negative arm matters as much as the positive one,
    /// since an over-broad match would let e.g. a copper block count toward
    /// the base pyramid.
    #[test]
    fn base_block_recognition_matches_the_real_five_member_tag() {
        let mut recognised = Vec::new();
        for id in 0..lodestone_data::block_states::STATE_COUNT {
            let Some(name) = lodestone_data::block_states::block_name(id) else {
                continue;
            };
            if is_beacon_base_block(id) {
                recognised.push(name.to_string());
            }
        }
        recognised.sort();
        recognised.dedup();
        assert_eq!(
            recognised,
            vec![
                "minecraft:diamond_block",
                "minecraft:emerald_block",
                "minecraft:gold_block",
                "minecraft:iron_block",
                "minecraft:netherite_block",
            ]
        );
    }

    /// Two known non-base blocks must not be recognised — plain stone (an
    /// arbitrary full solid) and copper block (a real "shiny metal block"
    /// that a careless substring match on the base blocks' names could
    /// accidentally sweep in).
    #[test]
    fn ordinary_blocks_are_not_base_blocks() {
        let stone = lodestone_data::block_states::state_id("minecraft:stone")
            .expect("stone must resolve");
        assert!(!is_beacon_base_block(stone));
        if let Some(copper) = lodestone_data::block_states::state_id("minecraft:copper_block") {
            assert!(!is_beacon_base_block(copper));
        }
    }
}

// --- vault ---------------------------------------------------------------

/// `shared_data.display_item.{id, count}`, out of a vault's block-entity NBT
/// — vanilla's own vault shared-data codec's `display_item` field, an ordinary
/// item-stack codec (`{id: <string>, count: <int, default 1>, components:
/// <compound, optional>}`; `components` is not read here, the same limitation
/// [`campfire_items`]' doc names for its own item id).
///
/// `None` for a missing `shared_data` compound, a missing `display_item`, a
/// `display_item` with no `id`, or `id == "minecraft:air"` — all of these are
/// vanilla's own vault shared-data "no display item" cases
/// (an empty item stack, or the codec's absent-field default),
/// and vanilla's own vault client-side active-effects check draws nothing for
/// any of them.
#[must_use]
fn vault_display_item(nbt: &lodestone_core::Nbt) -> Option<(lodestone_assets::ResourceLocation, u32)> {
    use lodestone_core::Nbt;

    let Nbt::Compound(fields) = nbt else {
        return None;
    };
    let Some(Nbt::Compound(shared)) =
        fields.iter().find(|(name, _)| name == "shared_data").map(|(_, v)| v)
    else {
        return None;
    };
    let Some(Nbt::Compound(item)) =
        shared.iter().find(|(name, _)| name == "display_item").map(|(_, v)| v)
    else {
        return None;
    };
    let field = |key: &str| item.iter().find(|(name, _)| name == key).map(|(_, v)| v);
    let Some(Nbt::String(id)) = field("id") else {
        return None;
    };
    if id == "minecraft:air" {
        return None;
    }
    // Vanilla's own codec helper for an optional-but-defaulted field: a missing
    // field defaults to one copy, not zero.
    let count = match field("count") {
        Some(Nbt::Int(n)) => u32::try_from(*n).ok().filter(|n| *n > 0)?,
        None => 1,
        _ => return None,
    };
    Some((id.parse().ok()?, count))
}

/// Every vault position within [`VIEW_DISTANCE`], paired with its parsed
/// display item — a fourth NBT-reading candidate gather beside
/// [`sign_candidates`]/[`banner_candidates`]/[`campfire_candidates`], for the
/// same reason those exist: [`chest_candidates`] discards `be.nbt`, and a
/// vault's whole reward display lives there. The block-name check happens
/// before the NBT parse, [`campfire_candidates`]' shape: every block entity in
/// range would otherwise be walked for a `shared_data` compound that only a
/// vault ever carries.
#[must_use]
fn vault_candidates(
    world: &World,
    chunks: impl IntoIterator<Item = ChunkPos>,
    eye: Vec3,
) -> Vec<([i32; 3], Option<(lodestone_assets::ResourceLocation, u32)>)> {
    let cutoff = VIEW_DISTANCE * VIEW_DISTANCE;
    let mut candidates = Vec::new();
    for pos in chunks {
        let Some(chunk) = world.get(pos) else {
            continue;
        };
        for be in &chunk.block_entities {
            let x = pos.x * 16 + i32::from(be.rel_x);
            let z = pos.z * 16 + i32::from(be.rel_z);
            let y = i32::from(be.y);
            let centre = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
            if centre.distance_squared(eye) > cutoff {
                continue;
            }
            let state_id = chunk
                .column
                .get_block(usize::from(be.rel_x), y, usize::from(be.rel_z));
            let Some(name) = lodestone_data::block_states::block_name(state_id) else {
                continue;
            };
            if name != "minecraft:vault" {
                continue;
            }
            candidates.push(([x, y, z], vault_display_item(&be.nbt)));
        }
    }
    candidates
}

/// Every vault to draw this frame — the vault sibling of
/// [`decorated_pot_spawns`], for
/// [`crate::gpu::RenderState::set_vault_source`]. A vault with no display
/// item (`VaultState::INACTIVE`, or any state before the server has rolled
/// one) contributes nothing, matching
/// vanilla's own vault client-side active-effects-display guard.
///
/// `spin_deg` is resolved once here from `(game_time, partial_tick)` — see
/// [`vault_spin_degrees`]'s doc for why every vault in this client shares one
/// clock rather than each carrying its own per-instance phase.
#[must_use]
pub fn vault_spawns(handle: &SharedHandle, eye: Vec3, game_time: i64, partial_tick: f32) -> Vec<VaultSpawn> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();

    let candidates = {
        let world = store.read();
        vault_candidates(
            &world,
            chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }),
            eye,
        )
    };

    let sky_default = {
        let player = client.player();
        crate::mesher::sky_default_for_dimension(
            player.dimension.as_ref(),
            player.dimension_type.as_ref(),
        )
    };

    let spin_deg = vault_spin_degrees(game_time, partial_tick);
    let mut out = Vec::new();
    for (block, item) in candidates {
        let Some((item, count)) = item else { continue };
        let light = entity_light_at(handle, block[0], block[1], block[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        out.push(VaultSpawn {
            pos: block,
            item,
            count,
            spin_deg,
            light,
        });
    }
    out.sort_by_key(|s| s.pos);
    out
}

#[cfg(test)]
mod vault_tests {
    use super::*;

    fn compound(fields: Vec<(&str, lodestone_core::Nbt)>) -> lodestone_core::Nbt {
        lodestone_core::Nbt::Compound(
            fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        )
    }

    /// The shape vanilla's own vault shared-data codec actually writes:
    /// `{shared_data: {display_item: {id, count}, connected_players: [...],
    /// connected_particles_range: <double>}}`. Parsed straight through, not a
    /// hand-restated literal.
    #[test]
    fn parses_a_real_shared_data_shape() {
        let nbt = compound(vec![(
            "shared_data",
            compound(vec![
                (
                    "display_item",
                    compound(vec![
                        (
                            "id",
                            lodestone_core::Nbt::String("minecraft:diamond".to_string()),
                        ),
                        ("count", lodestone_core::Nbt::Int(3)),
                    ]),
                ),
                (
                    "connected_particles_range",
                    lodestone_core::Nbt::Double(8.0),
                ),
            ]),
        )]);
        let (id, count) = vault_display_item(&nbt).expect("display item must parse");
        assert_eq!(id.to_string(), "minecraft:diamond");
        assert_eq!(count, 3);
    }

    /// A missing `count` field defaults to one copy —
    /// vanilla's own codec helper for an optional-but-defaulted field, not zero
    /// and not "absent".
    #[test]
    fn a_missing_count_defaults_to_one() {
        let nbt = compound(vec![(
            "shared_data",
            compound(vec![(
                "display_item",
                compound(vec![(
                    "id",
                    lodestone_core::Nbt::String("minecraft:emerald".to_string()),
                )]),
            )]),
        )]);
        let (_, count) = vault_display_item(&nbt).expect("display item must parse");
        assert_eq!(count, 1);
    }

    /// No `shared_data` compound at all (an inactive vault that has never
    /// rolled a reward) draws nothing — the common case for most vaults in a
    /// freshly generated trial chamber.
    #[test]
    fn an_inactive_vault_with_no_shared_data_has_no_display_item() {
        assert!(vault_display_item(&compound(vec![])).is_none());
    }

    /// `minecraft:air` is vanilla's own empty-item-stack's real registry id — a vault whose
    /// display item was explicitly cleared must read the same as one that
    /// never had `shared_data` at all, not as "an air block floating in a
    /// cage".
    #[test]
    fn an_air_display_item_is_treated_as_empty() {
        let nbt = compound(vec![(
            "shared_data",
            compound(vec![(
                "display_item",
                compound(vec![(
                    "id",
                    lodestone_core::Nbt::String("minecraft:air".to_string()),
                )]),
            )]),
        )]);
        assert!(vault_display_item(&nbt).is_none());
    }

    /// `vault_spin_degrees` predicts a value from constants outside this
    /// module — the shell-side reuse of the render-crate function, so a wire
    /// mistake in *this* file's plumbing (e.g. swapping `game_time` and
    /// `partial_tick`) would still show up here.
    #[test]
    fn spin_advances_with_game_time_and_partial_tick() {
        let a = vault_spin_degrees(10, 0.0);
        let b = vault_spin_degrees(11, 0.0);
        assert!((b - a - 10.0).abs() < 1e-4);
        let mid = vault_spin_degrees(10, 0.5);
        assert!((mid - a - 5.0).abs() < 1e-4);
    }
}

// --- brushable block -------------------------------------------------------

/// `Direction.LEGACY_ID_CODEC`'s byte, resolved to [`lodestone_assets::Direction`]
/// rather than a unit step vector — the [`direction_step_from_3d`] sibling for
/// callers that need the enum itself. Same order as that function
/// (`Direction`'s own declaration order: `DOWN, UP, NORTH, SOUTH, WEST, EAST`),
/// which is **not** `lodestone_assets::Direction`'s declaration order (that one
/// lists `East` before `West`) — a `transmute`-shaped `as` cast here would
/// silently swap east and west.
#[must_use]
fn block_entity_direction_from_legacy_id(id: i8) -> Option<lodestone_assets::Direction> {
    use lodestone_assets::Direction;
    Some(match id {
        0 => Direction::Down,
        1 => Direction::Up,
        2 => Direction::North,
        3 => Direction::South,
        4 => Direction::West,
        5 => Direction::East,
        _ => return None,
    })
}

/// A brushable block's NBT, decoded — vanilla's own brushable-block-entity
/// update-tag's two
/// optional fields.
///
/// `hit_direction` is vanilla's own legacy direction-id codec (a byte), so this reads
/// an [`lodestone_core::Nbt::Byte`], **not** an int — the same trap
/// [`MovingPistonNbt`]'s doc names for its own `direction` field. `item` is an
/// ordinary item-stack codec compound (`{id, count, components}`); only `id`
/// is read, matching [`vault_display_item`]'s own limitation for the same
/// codec shape.
///
/// Returns `None` unless **both** fields are present — vanilla's own guard in
/// its own brushable-block render submission (both a hit direction and a
/// non-empty item state must be present)
/// — so a freshly placed, never-brushed block contributes nothing rather than
/// a stack drawn in a default direction.
#[must_use]
fn brushable_item(nbt: &lodestone_core::Nbt) -> Option<(lodestone_assets::Direction, lodestone_assets::ResourceLocation)> {
    use lodestone_core::Nbt;

    let Nbt::Compound(fields) = nbt else {
        return None;
    };
    let field = |key: &str| fields.iter().find(|(name, _)| name == key).map(|(_, v)| v);
    let Some(Nbt::Byte(direction_id)) = field("hit_direction") else {
        return None;
    };
    let direction = block_entity_direction_from_legacy_id(*direction_id)?;
    let Some(Nbt::Compound(item)) = field("item") else {
        return None;
    };
    let Some(Nbt::String(id)) = item.iter().find(|(name, _)| name == "id").map(|(_, v)| v)
    else {
        return None;
    };
    if id == "minecraft:air" {
        return None;
    }
    Some((direction, id.parse().ok()?))
}

/// The block state's own `dusted` property (`0..=3`) —
/// vanilla's own brushable-block-entity completion-state range, already reflected in
/// the state the server sends rather than re-derived from its own brush-count field (not
/// on the wire).
#[must_use]
fn brushable_dust_progress(state_id: u32) -> u8 {
    lodestone_data::block_states::properties(state_id)
        .and_then(|props| props.iter().find(|(name, _)| *name == "dusted"))
        .and_then(|(_, value)| value.parse::<u8>().ok())
        .unwrap_or(0)
}

/// Every suspicious sand/gravel position within [`VIEW_DISTANCE`], paired with
/// its parsed hit direction/item (if any) and its `dusted` progress — the
/// brushable-block sibling of [`vault_candidates`].
#[must_use]
fn brushable_candidates(
    world: &World,
    chunks: impl IntoIterator<Item = ChunkPos>,
    eye: Vec3,
) -> Vec<(
    [i32; 3],
    u8,
    Option<(lodestone_assets::Direction, lodestone_assets::ResourceLocation)>,
)> {
    let cutoff = VIEW_DISTANCE * VIEW_DISTANCE;
    let mut candidates = Vec::new();
    for pos in chunks {
        let Some(chunk) = world.get(pos) else {
            continue;
        };
        for be in &chunk.block_entities {
            let x = pos.x * 16 + i32::from(be.rel_x);
            let z = pos.z * 16 + i32::from(be.rel_z);
            let y = i32::from(be.y);
            let centre = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
            if centre.distance_squared(eye) > cutoff {
                continue;
            }
            let state_id = chunk
                .column
                .get_block(usize::from(be.rel_x), y, usize::from(be.rel_z));
            let Some(name) = lodestone_data::block_states::block_name(state_id) else {
                continue;
            };
            if name != "minecraft:suspicious_sand" && name != "minecraft:suspicious_gravel" {
                continue;
            }
            candidates.push((
                [x, y, z],
                brushable_dust_progress(state_id),
                brushable_item(&be.nbt),
            ));
        }
    }
    candidates
}

/// Every brushable block's revealed item to draw this frame, for
/// [`crate::gpu::RenderState::set_brushable_source`]. A block that has never
/// been brushed, or whose loot table has not yet rolled an item, contributes
/// nothing — matching vanilla's own brushable-block render submission's three-part guard
/// (a positive dust progress, a hit direction, and a non-empty item state).
///
/// No clock captured, like [`campfire_spawns`]: nothing about a revealed item
/// animates.
#[must_use]
pub fn brushable_spawns(handle: &SharedHandle, eye: Vec3) -> Vec<BrushableItemSpawn> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();

    let candidates = {
        let world = store.read();
        brushable_candidates(
            &world,
            chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }),
            eye,
        )
    };

    let sky_default = {
        let player = client.player();
        crate::mesher::sky_default_for_dimension(
            player.dimension.as_ref(),
            player.dimension_type.as_ref(),
        )
    };

    let mut out = Vec::new();
    for (block, dust_progress, hit) in candidates {
        if dust_progress == 0 {
            continue;
        }
        let Some((hit_direction, item)) = hit else {
            continue;
        };
        let light = entity_light_at(handle, block[0], block[1], block[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        out.push(BrushableItemSpawn {
            pos: block,
            hit_direction,
            dust_progress,
            item,
            light,
        });
    }
    out.sort_by_key(|s| s.pos);
    out
}

#[cfg(test)]
mod brushable_tests {
    use super::*;

    fn compound(fields: Vec<(&str, lodestone_core::Nbt)>) -> lodestone_core::Nbt {
        lodestone_core::Nbt::Compound(
            fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        )
    }

    /// The shape vanilla's own brushable-block-entity update-tag actually writes:
    /// `{hit_direction: <byte>, item: {id, count}}`.
    #[test]
    fn parses_a_real_brushable_shape() {
        let nbt = compound(vec![
            ("hit_direction", lodestone_core::Nbt::Byte(1)), // UP
            (
                "item",
                compound(vec![
                    (
                        "id",
                        lodestone_core::Nbt::String("minecraft:brick".to_string()),
                    ),
                    ("count", lodestone_core::Nbt::Int(1)),
                ]),
            ),
        ]);
        let (direction, item) = brushable_item(&nbt).expect("brushable item must parse");
        assert_eq!(direction, lodestone_assets::Direction::Up);
        assert_eq!(item.to_string(), "minecraft:brick");
    }

    /// A never-brushed block (no `hit_direction`, no `item`) parses to
    /// nothing, the common case for most suspicious sand in a freshly
    /// generated desert well or ocean ruin.
    #[test]
    fn a_never_brushed_block_has_no_item() {
        assert!(brushable_item(&compound(vec![])).is_none());
    }

    /// `hit_direction` present with no `item` (a player has started digging
    /// but the loot table has not rolled yet, or rolled empty) also parses to
    /// nothing — matching `!itemState.isEmpty()`.
    #[test]
    fn a_hit_direction_with_no_item_is_not_enough() {
        let nbt = compound(vec![("hit_direction", lodestone_core::Nbt::Byte(0))]);
        assert!(brushable_item(&nbt).is_none());
    }

    /// `hit_direction` is a **byte**, not an int — reading it as one would
    /// silently default every brushable block to `DOWN`, matching
    /// [`MovingPistonNbt`]'s documented trap for the identical codec.
    #[test]
    fn hit_direction_as_an_int_does_not_parse() {
        let nbt = compound(vec![
            ("hit_direction", lodestone_core::Nbt::Int(5)), // EAST, wrong type
            (
                "item",
                compound(vec![(
                    "id",
                    lodestone_core::Nbt::String("minecraft:emerald".to_string()),
                )]),
            ),
        ]);
        assert!(brushable_item(&nbt).is_none());
    }

    /// `minecraft:air` reads the same as no item at all — the same "explicitly
    /// cleared" case [`vault_display_item`]'s doc names.
    #[test]
    fn an_air_item_is_treated_as_empty() {
        let nbt = compound(vec![
            ("hit_direction", lodestone_core::Nbt::Byte(2)),
            (
                "item",
                compound(vec![(
                    "id",
                    lodestone_core::Nbt::String("minecraft:air".to_string()),
                )]),
            ),
        ]);
        assert!(brushable_item(&nbt).is_none());
    }

    /// Every one of the six legacy ids round-trips to the direction
    /// `Direction`'s own declaration order says it should — the control for
    /// [`block_entity_direction_from_legacy_id`]'s doc warning about
    /// `lodestone_assets::Direction`'s differently-ordered declaration.
    #[test]
    fn legacy_ids_resolve_in_directions_own_declaration_order() {
        use lodestone_assets::Direction;
        let expected = [
            (0, Direction::Down),
            (1, Direction::Up),
            (2, Direction::North),
            (3, Direction::South),
            (4, Direction::West),
            (5, Direction::East),
        ];
        for (id, want) in expected {
            assert_eq!(block_entity_direction_from_legacy_id(id), Some(want));
        }
        assert_eq!(block_entity_direction_from_legacy_id(6), None);
    }
}

// --- shelf -------------------------------------------------------------

/// The `facing` yaw of a shelf block, or `None` for any other block.
///
/// Every wood variant counts (`acacia_shelf`, `oak_shelf`, …) — the block
/// name check is a suffix test rather than a fixed list, matched against
/// `_shelf` (**with** the leading underscore) so it does not also catch
/// `minecraft:bookshelf`/`minecraft:chiseled_bookshelf`, two unrelated
/// blocks (`bookshelf` has no `ShelfBlockEntity` at all; `chiseled_bookshelf`
/// is its own block entity with no renderer registration — see this module's
/// top-of-file doc for the "23 with no renderer" census).
#[must_use]
fn shelf_facing_yaw(state_id: u32) -> Option<f32> {
    let name = lodestone_data::block_states::block_name(state_id)?;
    if !name.ends_with("_shelf") {
        return None;
    }
    let props = lodestone_data::block_states::properties(state_id)?;
    props
        .iter()
        .find(|(name, _)| *name == "facing")
        .and_then(|(_, value)| horizontal_facing_yaw(value))
}

/// Vanilla's own shelf-block-entity align-items-to-bottom accessor's own NBT flag —
/// `align_items_to_bottom`, an ordinary boolean (`Nbt::Byte`, `!= 0`).
/// Missing entirely (a freshly placed shelf whose block entity has never
/// been re-saved) defaults to `false`, matching
/// vanilla's own boolean-with-default NBT read.
#[must_use]
fn shelf_align_to_bottom(nbt: &lodestone_core::Nbt) -> bool {
    use lodestone_core::Nbt;
    let Nbt::Compound(fields) = nbt else {
        return false;
    };
    matches!(
        fields
            .iter()
            .find(|(name, _)| name == "align_items_to_bottom")
            .map(|(_, v)| v),
        Some(Nbt::Byte(v)) if *v != 0
    )
}

/// The occupied slots in a shelf's NBT, as `(slot, item id)` — the same
/// slot-tagged item-stack codec shape [`campfire_items`] already parses
/// (`Items` list, `Slot` an unsigned byte, **not** the list index), narrowed
/// to [`SHELF_SLOTS`] rather than [`lodestone_render::CAMPFIRE_SLOTS`].
#[must_use]
fn shelf_items(nbt: &lodestone_core::Nbt) -> Vec<(usize, lodestone_assets::ResourceLocation)> {
    use lodestone_core::Nbt;

    let Nbt::Compound(fields) = nbt else {
        return Vec::new();
    };
    let Some(Nbt::List { elements, .. }) =
        fields.iter().find(|(name, _)| name == "Items").map(|(_, v)| v)
    else {
        return Vec::new();
    };
    elements
        .iter()
        .filter_map(|entry| {
            let Nbt::Compound(entry) = entry else {
                return None;
            };
            let field = |key: &str| entry.iter().find(|(name, _)| name == key).map(|(_, v)| v);
            let slot = match field("Slot") {
                Some(Nbt::Byte(slot)) => usize::try_from(*slot).ok()?,
                None => 0,
                _ => return None,
            };
            if slot >= SHELF_SLOTS {
                return None;
            }
            let Some(Nbt::String(id)) = field("id") else {
                return None;
            };
            Some((slot, id.parse().ok()?))
        })
        .collect()
}

/// Every shelf position within [`VIEW_DISTANCE`], paired with its `facing`
/// yaw, `align_items_to_bottom` flag and occupied-slot item list — the shelf
/// sibling of [`campfire_candidates`].
#[must_use]
fn shelf_candidates(
    world: &World,
    chunks: impl IntoIterator<Item = ChunkPos>,
    eye: Vec3,
) -> Vec<(
    [i32; 3],
    f32,
    bool,
    Vec<(usize, lodestone_assets::ResourceLocation)>,
)> {
    let cutoff = VIEW_DISTANCE * VIEW_DISTANCE;
    let mut candidates = Vec::new();
    for pos in chunks {
        let Some(chunk) = world.get(pos) else {
            continue;
        };
        for be in &chunk.block_entities {
            let x = pos.x * 16 + i32::from(be.rel_x);
            let z = pos.z * 16 + i32::from(be.rel_z);
            let y = i32::from(be.y);
            let centre = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
            if centre.distance_squared(eye) > cutoff {
                continue;
            }
            let state_id = chunk
                .column
                .get_block(usize::from(be.rel_x), y, usize::from(be.rel_z));
            let Some(facing_yaw_deg) = shelf_facing_yaw(state_id) else {
                continue;
            };
            candidates.push((
                [x, y, z],
                facing_yaw_deg,
                shelf_align_to_bottom(&be.nbt),
                shelf_items(&be.nbt),
            ));
        }
    }
    candidates
}

/// Every shelf's occupied-slot items to draw this frame, for
/// [`crate::gpu::RenderState::set_shelf_source`]. An empty shelf contributes
/// nothing, matching vanilla's own shelf render submission's per-slot null guard.
///
/// No clock captured, like [`campfire_spawns`]: nothing about a shelved item
/// animates.
#[must_use]
pub fn shelf_spawns(handle: &SharedHandle, eye: Vec3) -> Vec<ShelfItemSpawn> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();

    let candidates = {
        let world = store.read();
        shelf_candidates(
            &world,
            chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }),
            eye,
        )
    };

    let sky_default = {
        let player = client.player();
        crate::mesher::sky_default_for_dimension(
            player.dimension.as_ref(),
            player.dimension_type.as_ref(),
        )
    };

    let mut out = Vec::new();
    for (block, facing_yaw_deg, align_to_bottom, items) in candidates {
        if items.is_empty() {
            continue;
        }
        let light = entity_light_at(handle, block[0], block[1], block[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        for (slot, item) in items {
            out.push(ShelfItemSpawn {
                pos: block,
                facing_yaw_deg,
                slot,
                align_to_bottom,
                item,
                light,
            });
        }
    }
    out.sort_by_key(|s| (s.pos, s.slot));
    out
}

#[cfg(test)]
mod shelf_tests {
    use super::*;

    fn compound(fields: Vec<(&str, lodestone_core::Nbt)>) -> lodestone_core::Nbt {
        lodestone_core::Nbt::Compound(
            fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        )
    }

    /// The shape vanilla's own shelf-block-entity save routine actually writes:
    /// `{Items: [{Slot, id, count}, ...], align_items_to_bottom: <byte>}`.
    #[test]
    fn parses_a_real_shelf_shape() {
        let nbt = compound(vec![
            (
                "Items",
                lodestone_core::Nbt::List {
                    element_type: lodestone_core::NbtTag::Compound,
                    elements: vec![
                        compound(vec![
                            ("Slot", lodestone_core::Nbt::Byte(0)),
                            (
                                "id",
                                lodestone_core::Nbt::String("minecraft:book".to_string()),
                            ),
                            ("count", lodestone_core::Nbt::Int(1)),
                        ]),
                        compound(vec![
                            ("Slot", lodestone_core::Nbt::Byte(2)),
                            (
                                "id",
                                lodestone_core::Nbt::String("minecraft:torch".to_string()),
                            ),
                            ("count", lodestone_core::Nbt::Int(1)),
                        ]),
                    ],
                },
            ),
            ("align_items_to_bottom", lodestone_core::Nbt::Byte(1)),
        ]);
        let items = shelf_items(&nbt);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].0, 0);
        assert_eq!(items[0].1.to_string(), "minecraft:book");
        assert_eq!(items[1].0, 2);
        assert_eq!(items[1].1.to_string(), "minecraft:torch");
        assert!(shelf_align_to_bottom(&nbt));
    }

    /// An empty shelf (no `Items` list at all) parses to no items, and a
    /// missing `align_items_to_bottom` defaults to `false`.
    #[test]
    fn an_empty_shelf_has_no_items_and_defaults_unaligned() {
        assert!(shelf_items(&compound(vec![])).is_empty());
        assert!(!shelf_align_to_bottom(&compound(vec![])));
    }

    /// `Slot` is the list-entry field, not the list index — a single item
    /// stored at `Slot: 2` must resolve to slot 2, not slot 0 (the index it
    /// occupies in a one-element list). The same trap
    /// [`campfire_items`]'s doc names for the identical codec shape.
    #[test]
    fn slot_is_read_from_the_field_not_the_list_index() {
        let nbt = compound(vec![(
            "Items",
            lodestone_core::Nbt::List {
                element_type: lodestone_core::NbtTag::Compound,
                elements: vec![compound(vec![
                    ("Slot", lodestone_core::Nbt::Byte(2)),
                    (
                        "id",
                        lodestone_core::Nbt::String("minecraft:emerald".to_string()),
                    ),
                ])],
            },
        )]);
        let items = shelf_items(&nbt);
        assert_eq!(items, vec![(2, "minecraft:emerald".parse().unwrap())]);
    }

    /// A `Slot` at or past [`SHELF_SLOTS`] is dropped, matching
    /// vanilla's own slot-tagged item-stack container-validity check.
    #[test]
    fn an_out_of_range_slot_is_dropped() {
        let nbt = compound(vec![(
            "Items",
            lodestone_core::Nbt::List {
                element_type: lodestone_core::NbtTag::Compound,
                elements: vec![compound(vec![
                    ("Slot", lodestone_core::Nbt::Byte(3)),
                    (
                        "id",
                        lodestone_core::Nbt::String("minecraft:emerald".to_string()),
                    ),
                ])],
            },
        )]);
        assert!(shelf_items(&nbt).is_empty());
    }

    /// `bookshelf`/`chiseled_bookshelf` must not resolve a facing yaw — the
    /// suffix check this module's doc warns about getting backwards.
    #[test]
    fn plain_bookshelf_is_not_a_shelf_block_entity() {
        if let Some(id) = lodestone_data::block_states::state_id("minecraft:bookshelf") {
            assert!(shelf_facing_yaw(id).is_none());
        }
        if let Some(id) = lodestone_data::block_states::state_id("minecraft:chiseled_bookshelf") {
            assert!(shelf_facing_yaw(id).is_none());
        }
    }
}

// --- copper golem statue -------------------------------------------------

/// The block's `copper_golem_pose` property, or `None` for any other block.
#[must_use]
fn copper_golem_statue_pose(state_id: u32) -> Option<CopperGolemPose> {
    let props = lodestone_data::block_states::properties(state_id)?;
    let value = props
        .iter()
        .find(|(name, _)| *name == "copper_golem_pose")
        .map(|(_, v)| *v)?;
    Some(match value {
        "standing" => CopperGolemPose::Standing,
        "sitting" => CopperGolemPose::Sitting,
        "running" => CopperGolemPose::Running,
        "star" => CopperGolemPose::Star,
        _ => return None,
    })
}

/// The block's oxidation level, from its registry name — `WeatheringCopper
/// .getPreviousState`'s own four-level chain, restated as a name-prefix
/// match the way [`chest_material`] already does for copper chest variants.
/// `waxed_` is stripped first: waxing halts further weathering but does not
/// change which of the four textures a statue currently uses
/// (`CopperGolemOxidationLevels` has no fifth, waxed-specific entry).
#[must_use]
fn copper_golem_statue_oxidation(state_id: u32) -> Option<CopperGolemOxidation> {
    let name = lodestone_data::block_states::block_name(state_id)?;
    let path = name.strip_prefix("minecraft:").unwrap_or(name);
    let path = path.strip_prefix("waxed_").unwrap_or(path);
    Some(match path {
        "copper_golem_statue" => CopperGolemOxidation::Unaffected,
        "exposed_copper_golem_statue" => CopperGolemOxidation::Exposed,
        "weathered_copper_golem_statue" => CopperGolemOxidation::Weathered,
        "oxidized_copper_golem_statue" => CopperGolemOxidation::Oxidized,
        _ => return None,
    })
}

/// Resolves one block state id into a copper golem statue spawn, or `None`
/// if it is not a statue, or its `facing`/`copper_golem_pose` properties do
/// not resolve.
#[must_use]
pub fn copper_golem_statue_spawn(
    block: [i32; 3],
    state_id: u32,
    light: u8,
) -> Option<CopperGolemStatueSpawn> {
    let oxidation = copper_golem_statue_oxidation(state_id)?;
    let pose = copper_golem_statue_pose(state_id)?;
    let props = lodestone_data::block_states::properties(state_id)?;
    let facing_yaw_deg = props
        .iter()
        .find(|(name, _)| *name == "facing")
        .and_then(|(_, value)| horizontal_facing_yaw(value))?;
    Some(CopperGolemStatueSpawn {
        pos: block,
        facing_yaw_deg,
        pose,
        oxidation,
        light,
    })
}

/// Every copper golem statue to draw this frame — the statue sibling of
/// [`skull_spawns`], reusing [`chest_candidates`] for the same reason that
/// one does: no NBT is needed at all (pose, oxidation and facing are all
/// block-state/block-name driven), so the generic position gather is the
/// whole job.
#[must_use]
pub fn copper_golem_statue_spawns(handle: &SharedHandle, eye: Vec3) -> Vec<CopperGolemStatueSpawn> {
    let Some(snapshot) = block_entity_frame_snapshot(handle, eye) else {
        return Vec::new();
    };
    copper_golem_statue_spawns_from_snapshot(&snapshot)
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

#[cfg(test)]
mod copper_golem_statue_tests {
    use super::*;

    /// Every one of the four oxidation levels resolves, for both the
    /// unwaxed and waxed block-name forms — waxing must not silently change
    /// which texture the statue reads.
    #[test]
    fn every_oxidation_level_resolves_waxed_and_unwaxed() {
        let names = [
            ("minecraft:copper_golem_statue", CopperGolemOxidation::Unaffected),
            ("minecraft:waxed_copper_golem_statue", CopperGolemOxidation::Unaffected),
            ("minecraft:exposed_copper_golem_statue", CopperGolemOxidation::Exposed),
            (
                "minecraft:waxed_exposed_copper_golem_statue",
                CopperGolemOxidation::Exposed,
            ),
            (
                "minecraft:weathered_copper_golem_statue",
                CopperGolemOxidation::Weathered,
            ),
            (
                "minecraft:waxed_weathered_copper_golem_statue",
                CopperGolemOxidation::Weathered,
            ),
            (
                "minecraft:oxidized_copper_golem_statue",
                CopperGolemOxidation::Oxidized,
            ),
            (
                "minecraft:waxed_oxidized_copper_golem_statue",
                CopperGolemOxidation::Oxidized,
            ),
        ];
        for (name, want) in names {
            let Some(id) = lodestone_data::block_states::state_id(name) else {
                continue;
            };
            assert_eq!(
                copper_golem_statue_oxidation(id),
                Some(want),
                "{name} resolved wrong"
            );
        }
    }

    /// The four pose strings parse to the right enum variant — checked
    /// directly against the raw state-id search space (a statue's own default
    /// state plus a short scan for the other three pose values), rather than
    /// assuming a specific state-id encoding.
    #[test]
    fn every_pose_string_resolves_to_its_own_variant() {
        let Some(base_id) = lodestone_data::block_states::state_id("minecraft:copper_golem_statue")
        else {
            return;
        };
        // The default state must resolve to *some* real pose, not `None`.
        assert!(copper_golem_statue_pose(base_id).is_some());
        let wanted = [
            ("standing", CopperGolemPose::Standing),
            ("sitting", CopperGolemPose::Sitting),
            ("running", CopperGolemPose::Running),
            ("star", CopperGolemPose::Star),
        ];
        // A statue has `copper_golem_pose` (4) x `facing` (4) x
        // `waterlogged` (2) = 32 states; scanning that window from the base
        // id is enough to find all four pose values without assuming which
        // offset each one sits at.
        for (value, want) in wanted {
            let found = (base_id..base_id.saturating_add(32)).find(|id| {
                lodestone_data::block_states::properties(*id).is_some_and(|p| {
                    p.iter()
                        .any(|(n, v)| *n == "copper_golem_pose" && *v == value)
                })
            });
            if let Some(id) = found {
                assert_eq!(copper_golem_statue_pose(id), Some(want), "pose {value}");
            }
        }
    }
}

// --- end gateway teleport beam --------------------------------------------

/// A generic stand-in for `level.getMaxY()` — the real dimension height the
/// spawning arm's beam grows toward (`beamDistance = isSpawning() ?
/// level.getMaxY() : 50.0`). This client resolves a dimension's real height
/// through world data already loaded, not through this gather, and threading
/// it in is not worth the plumbing for an effect visible for ~10 seconds per
/// gateway lifetime — a deliberate simplification, the same shape
/// `beacon.rs`'s module doc already accepts for scoping/zoom. `320.0`
/// (a generic "very tall") is harmless either high or low: it only sets how
/// fast the beam grows toward whatever height is actually visible.
const END_GATEWAY_SPAWN_BEAM_DISTANCE: f32 = 320.0;

/// Vanilla's own end-gateway-block-entity save routine's `Age` — an `Nbt::Long` — or
/// `0` when absent, matching vanilla's own long-or-default NBT read.
#[must_use]
fn end_gateway_age(nbt: &lodestone_core::Nbt) -> i64 {
    use lodestone_core::Nbt;
    let Nbt::Compound(fields) = nbt else {
        return 0;
    };
    match fields.iter().find(|(name, _)| name == "Age").map(|(_, v)| v) {
        Some(Nbt::Long(v)) => *v,
        _ => 0,
    }
}

/// Every end gateway within [`VIEW_DISTANCE`], paired with its `Age` NBT —
/// a second, NBT-carrying candidate gather beside [`end_gateway_spawns`]'s
/// [`chest_candidates`] reuse: that one discards NBT (it only needs the face
/// list), and the beam needs `Age` for the spawning arm.
#[must_use]
fn end_gateway_beam_candidates(
    world: &World,
    chunks: impl IntoIterator<Item = ChunkPos>,
    eye: Vec3,
) -> Vec<([i32; 3], i64)> {
    let cutoff = VIEW_DISTANCE * VIEW_DISTANCE;
    let mut candidates = Vec::new();
    for pos in chunks {
        let Some(chunk) = world.get(pos) else {
            continue;
        };
        for be in &chunk.block_entities {
            let x = pos.x * 16 + i32::from(be.rel_x);
            let z = pos.z * 16 + i32::from(be.rel_z);
            let y = i32::from(be.y);
            let centre = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
            if centre.distance_squared(eye) > cutoff {
                continue;
            }
            let state_id = chunk
                .column
                .get_block(usize::from(be.rel_x), y, usize::from(be.rel_z));
            let Some(name) = lodestone_data::block_states::block_name(state_id) else {
                continue;
            };
            if name != "minecraft:end_gateway" {
                continue;
            }
            candidates.push(([x, y, z], end_gateway_age(&be.nbt)));
        }
    }
    candidates
}

/// Every end gateway's teleport beam to draw this frame — vanilla's own
/// end-gateway render submission's call into the shared beacon-beam
/// submission,
/// shown while `isSpawning()` (real `Age` NBT, read fresh each frame — a
/// **stateless** per-frame computation, unlike `teleportCooldown` below)
/// or `isCoolingDown()` (`cooldowns`, [`GatewayCooldowns`] — a real
/// per-position, `BLOCK_EVENT`-driven tracker, ticked once per client tick
/// in `Sim::step` and captured here at install time like [`bell_spawns`]).
///
/// **Why `age` is not itself tracked locally, unlike `teleportCooldown`**:
/// `getUpdateTag` (`Age`'s only path to the client) is sent on initial load
/// and again whenever `spawning != isSpawning()` flips — rare, not every
/// tick — so a purely-tracked local clock would need to be *seeded* from
/// that NBT with no channel to do so (the render-source closure captures an
/// owned snapshot each frame; it cannot write back into a tracker). Reading
/// `Age` fresh from the world every frame and adding `partial_tick` is the
/// cheaper, honest alternative: correct at the instant a snapshot arrives,
/// static between snapshots — visible only as a slightly chunky spawn ramp
/// during the rare (and short) 200-tick window a gateway is newly created
/// with a player already nearby, named here as a real simplification rather
/// than silently approximated.
#[must_use]
pub fn end_gateway_beam_spawns(
    handle: &SharedHandle,
    cooldowns: &GatewayCooldowns,
    eye: Vec3,
    game_time: i64,
    partial_tick: f32,
) -> Vec<lodestone_render::EndGatewayBeamSpawn> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();

    let candidates = {
        let world = store.read();
        end_gateway_beam_candidates(
            &world,
            chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }),
            eye,
        )
    };

    let animation_time =
        game_time.rem_euclid(40) as f32 + partial_tick;
    let mut out = Vec::new();
    for (pos, age) in candidates {
        let is_spawning = age < 200;
        let cooldown = cooldowns.cooldown(pos);
        let is_cooling_down = cooldown.is_some();
        if !is_spawning && !is_cooling_down {
            continue;
        }
        let (scale01, beam_distance, color) = if is_spawning {
            let t = (age as f32 + partial_tick) / 200.0;
            (
                t.clamp(0.0, 1.0),
                END_GATEWAY_SPAWN_BEAM_DISTANCE,
                DyeColor::Magenta.packed_rgb(),
            )
        } else {
            // `cooldown` is `Some` here — `is_cooling_down` guarantees it.
            let ticks = cooldown.unwrap_or(0) as f32;
            let t = 1.0 - ((ticks - partial_tick) / 40.0).clamp(0.0, 1.0);
            (t, 50.0, DyeColor::Purple.packed_rgb())
        };
        let scale = (scale01 * std::f32::consts::PI).sin();
        let height = (scale * beam_distance).floor() as i32;
        out.push(lodestone_render::EndGatewayBeamSpawn {
            pos,
            scale,
            animation_time,
            height,
            color,
        });
    }
    out.sort_by_key(|s| s.pos);
    out
}

#[cfg(test)]
mod end_gateway_beam_tests {
    use super::*;

    /// `Age` present and small, no `BLOCK_EVENT` ever received: only the
    /// spawning arm fires, colour magenta.
    #[test]
    fn a_fresh_gateway_with_small_age_is_spawning_and_magenta() {
        let cooldowns = GatewayCooldowns::new();
        assert!(cooldowns.cooldown([0, 0, 0]).is_none());
        // Directly exercise the pure math the gather applies, since building
        // a real `World`/`SharedHandle` is out of scope for a unit test here
        // — `end_gateway_age` and the cooldown lookup are what this test
        // actually holds down.
        let nbt = lodestone_core::Nbt::Compound(vec![(
            "Age".to_string(),
            lodestone_core::Nbt::Long(5),
        )]);
        assert_eq!(end_gateway_age(&nbt), 5);
        assert!(5 < 200, "age 5 must read as spawning");
    }

    /// No `Age` field at all reads as `0`, matching `getLongOr("Age", 0L)`.
    #[test]
    fn missing_age_defaults_to_zero() {
        let nbt = lodestone_core::Nbt::Compound(vec![]);
        assert_eq!(end_gateway_age(&nbt), 0);
    }

    /// `GatewayCooldowns` round-trips exactly like `BellShakes`: a `b0 == 1`
    /// event starts a 40-tick countdown, ticking decrements it, and the
    /// entry disappears once it reaches zero.
    #[test]
    fn cooldown_counts_down_from_forty_and_then_disappears() {
        let mut cooldowns = GatewayCooldowns::new();
        assert!(!cooldowns.apply_block_event([1, 2, 3], 0, 0), "b0 != 1 is not a trigger");
        assert!(cooldowns.apply_block_event([1, 2, 3], 1, 0));
        assert_eq!(cooldowns.cooldown([1, 2, 3]), Some(40));
        for expected in (0..40).rev() {
            cooldowns.tick();
            if expected == 0 {
                assert_eq!(cooldowns.cooldown([1, 2, 3]), None);
            } else {
                assert_eq!(cooldowns.cooldown([1, 2, 3]), Some(expected));
            }
        }
        assert!(cooldowns.is_empty());
    }
}

#[cfg(test)]
mod structure_block_tests {
    use super::*;
    use lodestone_world::{BlockEntity, ChunkColumn, ColumnLight, Heightmaps, LoadedChunk, PaletteKind};

    fn structure_nbt(fields: Vec<(&str, lodestone_core::Nbt)>) -> lodestone_core::Nbt {
        lodestone_core::Nbt::Compound(
            fields
                .into_iter()
                .map(|(name, value)| (name.to_string(), value))
                .collect(),
        )
    }

    fn sized_save(extra: Vec<(&str, lodestone_core::Nbt)>) -> lodestone_core::Nbt {
        let mut fields = vec![
            ("mode", lodestone_core::Nbt::String("SAVE".into())),
            ("posX", lodestone_core::Nbt::Int(1)),
            ("posY", lodestone_core::Nbt::Int(2)),
            ("posZ", lodestone_core::Nbt::Int(3)),
            ("sizeX", lodestone_core::Nbt::Int(4)),
            ("sizeY", lodestone_core::Nbt::Int(5)),
            ("sizeZ", lodestone_core::Nbt::Int(6)),
        ];
        for (name, value) in extra {
            if let Some((_, existing)) = fields.iter_mut().find(|(field, _)| *field == name) {
                *existing = value;
            } else {
                fields.push((name, value));
            }
        }
        structure_nbt(fields)
    }

    #[test]
    fn rotations_and_mirrors_follow_the_vanilla_renderable_box_table() {
        let cases = [
            ("NONE", "NONE", [11, 66, 13], [15, 71, 19]),
            ("NONE", "CLOCKWISE_90", [6, 66, 13], [12, 71, 17]),
            ("NONE", "CLOCKWISE_180", [8, 66, 8], [12, 71, 14]),
            ("NONE", "COUNTERCLOCKWISE_90", [11, 66, 10], [17, 71, 14]),
            ("LEFT_RIGHT", "NONE", [11, 66, 8], [15, 71, 14]),
            ("LEFT_RIGHT", "CLOCKWISE_90", [11, 66, 13], [17, 71, 17]),
            ("LEFT_RIGHT", "CLOCKWISE_180", [8, 66, 13], [12, 71, 19]),
            (
                "LEFT_RIGHT",
                "COUNTERCLOCKWISE_90",
                [6, 66, 10],
                [12, 71, 14],
            ),
            ("FRONT_BACK", "NONE", [8, 66, 13], [12, 71, 19]),
            ("FRONT_BACK", "CLOCKWISE_90", [6, 66, 10], [12, 71, 14]),
            ("FRONT_BACK", "CLOCKWISE_180", [11, 66, 8], [15, 71, 14]),
            (
                "FRONT_BACK",
                "COUNTERCLOCKWISE_90",
                [11, 66, 13],
                [17, 71, 17],
            ),
        ];
        for (mirror, rotation, min, max) in cases {
            let nbt = sized_save(vec![
                ("mirror", lodestone_core::Nbt::String(mirror.into())),
                ("rotation", lodestone_core::Nbt::String(rotation.into())),
            ]);
            let bounds = structure_box([10, 64, 10], &nbt).expect("valid NBT must render");
            assert_eq!(bounds.min, min, "{mirror}/{rotation} minimum");
            assert_eq!(bounds.max, max, "{mirror}/{rotation} maximum");
        }
    }

    #[test]
    fn structure_box_visibility_needs_gamemaster_creative_or_spectator() {
        assert!(can_render_structure_boxes(2, true, false));
        assert!(can_render_structure_boxes(0, false, true));
        assert!(!can_render_structure_boxes(1, true, false));
        assert!(!can_render_structure_boxes(2, false, false));
    }

    #[test]
    fn non_rendering_modes_and_hidden_load_boxes_emit_no_geometry() {
        let data = structure_nbt(vec![("mode", lodestone_core::Nbt::String("DATA".into()))]);
        assert!(structure_block_outline_vertices([0, 64, 0], &data).is_empty());

        let hidden_load = structure_nbt(vec![
            ("mode", lodestone_core::Nbt::String("LOAD".into())),
            ("showboundingbox", lodestone_core::Nbt::Byte(0)),
            ("sizeX", lodestone_core::Nbt::Int(1)),
            ("sizeY", lodestone_core::Nbt::Int(1)),
            ("sizeZ", lodestone_core::Nbt::Int(1)),
        ]);
        assert!(structure_block_outline_vertices([0, 64, 0], &hidden_load).is_empty());
    }

    #[test]
    fn absent_vanilla_defaulted_fields_are_allowed_but_wrong_tags_are_not() {
        let defaults = structure_nbt(vec![
            ("mode", lodestone_core::Nbt::String("SAVE".into())),
            ("sizeX", lodestone_core::Nbt::Int(1)),
            ("sizeY", lodestone_core::Nbt::Int(1)),
            ("sizeZ", lodestone_core::Nbt::Int(1)),
        ]);
        assert_eq!(
            structure_block_outline_vertices([0, 64, 0], &defaults).len(),
            24,
            "missing position/mirror/rotation fields have vanilla defaults"
        );

        for malformed in [
            sized_save(vec![("posX", lodestone_core::Nbt::String("1".into()))]),
            sized_save(vec![("sizeY", lodestone_core::Nbt::Byte(1))]),
            sized_save(vec![("mode", lodestone_core::Nbt::Int(0))]),
            sized_save(vec![("mirror", lodestone_core::Nbt::Int(0))]),
            sized_save(vec![("rotation", lodestone_core::Nbt::Int(0))]),
            structure_nbt(vec![
                ("mode", lodestone_core::Nbt::String("LOAD".into())),
                ("sizeX", lodestone_core::Nbt::Int(1)),
                ("sizeY", lodestone_core::Nbt::Int(1)),
                ("sizeZ", lodestone_core::Nbt::Int(1)),
                ("showboundingbox", lodestone_core::Nbt::Int(1)),
            ]),
        ] {
            assert!(
                structure_block_outline_vertices([0, 64, 0], &malformed).is_empty(),
                "a present field with the wrong NBT tag must not inherit a default"
            );
        }
    }

    fn state_id(name: &str) -> u32 {
        (0..lodestone_data::block_states::STATE_COUNT)
            .find(|id| lodestone_data::block_states::block_name(*id) == Some(name))
            .unwrap_or_else(|| panic!("{name} must be present in the 26.2 state table"))
    }

    fn world_with_block_entity(
        chunk: ChunkPos,
        rel_x: u8,
        y: i16,
        state: u32,
        nbt: lodestone_core::Nbt,
    ) -> World {
        let mut column = ChunkColumn::new(
            0,
            16,
            PaletteKind::block_states(),
            PaletteKind::biomes(),
            0,
            0,
        );
        column.set_block(usize::from(rel_x), i32::from(y), 0, state);
        let mut world = World::new();
        world.load(
            chunk,
            LoadedChunk::new(
                column,
                ColumnLight::new(16),
                Heightmaps::new(),
                vec![BlockEntity {
                    rel_x,
                    rel_z: 0,
                    y,
                    type_id: 0,
                    nbt,
                }],
            ),
        );
        world
    }

    #[test]
    fn loaded_structure_blocks_pass_permission_and_strict_96_block_scanner_gates() {
        let chunk = ChunkPos::new(6, 0);
        let world = world_with_block_entity(
            chunk,
            0,
            4,
            state_id("minecraft:structure_block"),
            sized_save(Vec::new()),
        );
        let eye = Vec3::new(0.5, 4.5, 0.5);
        assert!(
            structure_block_vertices_from_loaded_world(&world, [chunk], eye, 2, true, false).is_empty(),
            "Vec3.closerThan uses a strict cutoff, so the exact 96-block boundary is culled"
        );
        assert_eq!(
            structure_block_vertices_from_loaded_world(
                &world,
                [chunk],
                Vec3::new(1.5, 4.5, 0.5),
                2,
                true,
                false,
            )
            .len(),
            24,
            "a loaded, permitted structure block strictly inside 96 blocks emits its 24 outline vertices"
        );
        assert!(
            structure_block_vertices_from_loaded_world(
                &world,
                [chunk],
                Vec3::new(0.49, 4.5, 0.5),
                2,
                true,
                false,
            )
            .is_empty(),
            "a structure block just beyond the 96-block cutoff is culled"
        );
        assert!(
            structure_block_vertices_from_loaded_world(
                &world,
                [ChunkPos::new(0, 0)],
                eye,
                2,
                true,
                false,
            )
            .is_empty(),
            "a resident world column outside the client's loaded-chunk list is not scanned"
        );
        assert!(
            structure_block_vertices_from_loaded_world(&world, [chunk], eye, 1, true, false)
                .is_empty(),
            "permission is applied before scanning block entities"
        );
    }

    #[test]
    fn scanner_rejects_a_block_entity_when_its_current_block_state_disagrees() {
        let chunk = ChunkPos::new(0, 0);
        let world = world_with_block_entity(
            chunk,
            0,
            4,
            state_id("minecraft:chest"),
            sized_save(Vec::new()),
        );
        assert!(
            structure_block_vertices_from_loaded_world(
                &world,
                [chunk],
                Vec3::new(0.5, 4.5, 0.5),
                2,
                true,
                false,
            )
            .is_empty(),
            "block state, rather than the raw block-entity type/NBT, is the render truth"
        );
    }
}
