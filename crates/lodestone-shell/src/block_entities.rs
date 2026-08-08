//! The shell's block-entity source: turns the client-owned world's decoded
//! block-entity records into the render crate's [`ChestSpawn`]s, and owns the
//! chest-lid animation state that no other layer has anywhere to put.
//!
//! This is the **consumer end** of a chain that already existed and reached
//! nothing (issue #23). Before this module the chain stopped one hop short of
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
//! ([`crate::sim::write_predicted_block`], issue #381) and it is **not** a packet:
//! it is what stops a placed chest from being a hole for one server round trip.
//! See `docs/block-placement-prediction.md`.
//!
//! # There are **four** creation routes, not two (issue #374)
//!
//! The first version of that diagram listed only the chunk packet and
//! `block_entity_data`, which was accurate and read as exhaustive. It was not:
//! in vanilla, **writing a block state is what creates a block entity** — no
//! packet involved (26.2 `LevelChunk.java:341`,
//! `((EntityBlock)newBlock).newBlockEntity(pos, state)`) — and
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
//! #374 wired that into the two *packet* arms only, which left the same bug on the
//! **prediction** side — the client wrote no state at all on a right-click, so a
//! chest you placed did not exist locally until `BLOCK_UPDATE` came back (issue
//! #381). [`crate::sim::write_predicted_block`] closes it with the same pair, and
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
//!   is stale as of issue #374; the type is still the wrong question.)
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
//! `b0 == 1` and `b1 == viewer count` (`ChestBlockEntity.triggerEvent`, 26.2:
//! `if (b0 == 1) { this.chestLidController.shouldBeOpen(b1 > 0); }`), and the
//! *client* integrates that into an angle over the following ticks. So the
//! authoritative value is a client-side accumulator, and [`ChestLids`] is a
//! direct port of `ChestLidController`:
//!
//! * `tickLid()` ramps `openness` by **±0.1 per tick**, clamped to `0..=1`.
//! * `getOpenness(a)` is `lerp(a, oOpenness, openness)` — the *previous* tick's
//!   value interpolated toward the current one by the partial tick.
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

use glam::Vec3;
use lodestone_render::{
    BannerSpawn, BellShakeDirection, BellSpawn, ChestHalf, ChestMaterial, ChestSpawn,
    SHULKER_COLOURS, ShulkerFacing, ShulkerSpawn, SignKind, SignOrientation, SignSpawn,
    SkullOrientation, SkullSpawn, SkullType, horizontal_facing_yaw,
};
use lodestone_render::banner_pattern::{DyeColor, StoredPatternLayer};
use lodestone_world::{ChunkPos, SignText, World};

use crate::net::{SharedHandle, entity_light_at};

/// Vanilla's per-renderer cutoff: `BlockEntityRenderer.getViewDistance()`
/// returns `64`, and `shouldRender` compares it against the distance from the
/// camera to `Vec3.atCenterOf(blockPos)` — the block **centre**, not its corner.
///
/// Ported as the real thing rather than "the render distance" because it is
/// genuinely a fixed 64 blocks in vanilla regardless of the video setting, and
/// because the `atCenterOf` offset is the difference between a chest popping in
/// at 64.0 and at 63.1.
pub const VIEW_DISTANCE: f32 = 64.0;

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

    /// Advances every lid one client tick — `ChestLidController.tickLid()`.
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

    /// The interpolated openness at `pos` — `ChestLidController.getOpenness(a)`,
    /// i.e. `lerp(partial_tick, oOpenness, openness)`.
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

/// `BellBlockEntity.DURATION` — a shake runs 50 ticks and then stops
/// (`BellBlockEntity.tick`: `if (entity.ticks >= 50) { shaking = false; ticks = 0; }`).
const BELL_SHAKE_DURATION: f32 = 50.0;

/// One bell's shake — `BellBlockEntity`'s `clickDirection` plus its `ticks`
/// counter.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Shake {
    direction: BellShakeDirection,
    /// `BellBlockEntity.ticks`, counted up from `0`.
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
    /// `BellBlockEntity.triggerEvent` (`:43-53`): only `b0 == 1` is a bell ring,
    /// and `b1` is `Direction.from3DDataValue(...)` — the *face the bell was hit
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
            // `Direction.from3DDataValue` gives UP/DOWN for `0`/`1`, which
            // `BellModel.setupAnim` has no rotation for — vanilla stores it and
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
    /// `BellRenderer.extractRenderState` passes into `setupAnim` — `ticks +
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

/// `Direction.from3DDataValue(b1)`, narrowed to the four horizontal directions
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
fn chest_orientation(state_id: u32) -> Option<(f32, ChestHalf)> {
    let props = lodestone_data::block_states::properties(state_id)?;
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
fn chest_material(state_id: u32) -> Option<ChestMaterial> {
    let name = lodestone_data::block_states::block_name(state_id)?;
    let path = name.strip_prefix("minecraft:").unwrap_or(name);
    ChestMaterial::from_block_path(path)
}

/// Every block-entity position within [`VIEW_DISTANCE`] of `eye`, paired with the
/// block state actually at it — the candidate set [`chest_spawns`] filters.
///
/// Split out of [`chest_spawns`] so a gate can drive the real gather against a
/// real [`World`] without a live `ClientHandle`: this is the loop that reads
/// `chunk.block_entities`, and therefore the loop that saw nothing at all before
/// issue #374 was fixed. Everything `chest_spawns` adds on top of this and
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
    state_id: u32,
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
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let mut out = Vec::new();

    // `loaded_chunks()` takes the world's read lock itself, so it is called
    // *before* the guard below rather than inside it. `std::sync::RwLock` gives
    // no re-entrancy guarantee — a nested read is allowed to deadlock once a
    // writer is queued, which on this world happens every time a chunk packet
    // lands. Taking it twice would produce a hang under load and never in a test.
    let chunks = client.loaded_chunks();

    // Then one read lock for the whole gather. The guard is dropped before the
    // light-sampling loop below, for exactly the same reason:
    // `entity_light_at` reaches for the same lock.
    let candidates = {
        let world = store.read();
        // `loaded_chunks` speaks `lodestone_model::ChunkPos`; the world is keyed by
        // `lodestone_world::ChunkPos`. Same two fields, distinct types.
        chest_candidates(
            &world,
            chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }),
            eye,
        )
    };

    // Resolved once for the whole frame, from the `client` this function already
    // holds, rather than per chest: `player()` clones a snapshot behind an ECS read
    // lock, and the loop below runs once per visible chest. The point samplers on
    // the render thread read a shared cell instead because they are `'static`
    // closures with no per-frame value available — see `net::SkyDefaultCell`.
    let sky_default = {
        let player = client.player();
        crate::mesher::sky_default_for_dimension(
            player.dimension.as_ref(),
            player.dimension_type.as_ref(),
        )
    };

    for (block, state_id) in candidates {
        // The light sample is the only thing here that needs the handle, which is
        // why it is the only thing `chest_candidates`/`chest_spawn` do not cover.
        let light = entity_light_at(handle, block[0], block[1], block[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        if let Some(spawn) =
            chest_spawn(block, state_id, lids.openness(block, partial_tick), light)
        {
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
fn skull_orientation(state_id: u32) -> Option<SkullOrientation> {
    let props = lodestone_data::block_states::properties(state_id)?;
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

/// Resolves one block state id into a skull/head type, or `None` if it is not
/// one of the five ported types (see
/// [`lodestone_render::SkullType::from_block_path`] for what is declined) —
/// including not being a skull at all.
#[must_use]
fn skull_type_for_state(state_id: u32) -> Option<SkullType> {
    let name = lodestone_data::block_states::block_name(state_id)?;
    let path = name.strip_prefix("minecraft:").unwrap_or(name);
    SkullType::from_block_path(path)
}

/// One candidate resolved into a [`SkullSpawn`], or `None` if the state at
/// that position is not a ported skull type.
///
/// Same shape as [`chest_spawn`]: the block **state** is the truth about
/// appearance, the block-entity record only says the position is worth
/// looking at, so a stale or orphan record whose state is not a skull draws
/// nothing.
#[must_use]
pub fn skull_spawn(block: [i32; 3], state_id: u32, light: u8) -> Option<SkullSpawn> {
    let skull_type = skull_type_for_state(state_id)?;
    let orientation = skull_orientation(state_id)?;
    Some(SkullSpawn {
        pos: block,
        orientation,
        skull_type,
        light,
    })
}

/// Every skull/head to draw this frame, gathered from the client-owned
/// world's block-entity records.
///
/// Reuses [`chest_candidates`] rather than a second scan of
/// `chunk.block_entities`: that gather is already generic over block-entity
/// *type* (it filters by distance and returns the raw state id, never
/// touching anything chest-specific), so a second copy here would only be
/// able to drift from it. Everything this adds on top is skull-specific
/// resolution and the light sample, the same division [`chest_spawns`] keeps.
///
/// No lid-style animation state: none of the five ported skull types pose
/// their head (see [`lodestone_render::BlockEntityModelSet::resolve_skull`]'s
/// doc), so there is nothing here to tick.
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
        chest_candidates(
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
    for (block, state_id) in candidates {
        let light = entity_light_at(handle, block[0], block[1], block[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        if let Some(spawn) = skull_spawn(block, state_id, light) {
            out.push(spawn);
        }
    }
    out.sort_by_key(|s| s.pos);
    out
}

/// Resolves one block state id into whether it names a bell — `None` for
/// anything else. Unlike [`chest_material`]/[`skull_type_for_state`] there is
/// no per-block-path variant to select: every bell block state (any
/// `FACING`/`ATTACHMENT`/`POWERED` combination) draws the identical rig, so
/// this only needs to confirm the block *is* one.
#[must_use]
fn bell_is_present(state_id: u32) -> bool {
    let Some(name) = lodestone_data::block_states::block_name(state_id) else {
        return false;
    };
    name == "minecraft:bell"
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
    state_id: u32,
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
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();

    let chunks = client.loaded_chunks();

    let candidates = {
        let world = store.read();
        chest_candidates(
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
    for (block, state_id) in candidates {
        let light = entity_light_at(handle, block[0], block[1], block[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        if let Some(spawn) = bell_spawn(block, state_id, light, shakes, partial_tick) {
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
/// is `ShulkerBoxRenderer.extractRenderState`'s own `getValueOrElse(FACING, UP)`
/// — unlike a chest, where a missing `facing` is treated as a failure, because a
/// shulker box genuinely has a sensible default and vanilla uses it.
#[must_use]
fn shulker_orientation(state_id: u32) -> Option<(Option<&'static str>, ShulkerFacing)> {
    let name = lodestone_data::block_states::block_name(state_id)?;
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
    if let Some(props) = lodestone_data::block_states::properties(state_id) {
        for (name, value) in props {
            if *name == "facing"
                && let Some(parsed) = ShulkerFacing::from_name(value)
            {
                facing = parsed;
            }
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
pub fn shulker_spawn(block: [i32; 3], state_id: u32, light: u8) -> Option<ShulkerSpawn> {
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
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();

    // The guard is taken and dropped *inside* this block, before the light
    // sampling below — the no-nested-read-lock rule every gather here follows.
    let candidates = {
        let world = store.read();
        chest_candidates(
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
    for (block, state_id) in candidates {
        let light = entity_light_at(handle, block[0], block[1], block[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        if let Some(spawn) = shulker_spawn(block, state_id, light) {
            out.push(spawn);
        }
    }
    out.sort_by_key(|s| s.pos);
    out
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
#[must_use]
fn sign_kind_for_state(state_id: u32) -> Option<SignKind> {
    let name = lodestone_data::block_states::block_name(state_id)?;
    let path = name.strip_prefix("minecraft:").unwrap_or(name);
    sign_kind_for_path(path)
}

/// Reads a plain sign's placement — `rotation` (`0..16`, ground) or `facing`
/// (wall) — into [`SignOrientation`]. Mirrors [`skull_orientation`] exactly:
/// a real sign state carries exactly one of the two (`oak_sign.json` has
/// `rotation`, `oak_wall_sign.json` has `facing`), and `None` for a state
/// with neither cannot happen for a real sign.
#[must_use]
fn sign_orientation(state_id: u32) -> Option<SignOrientation> {
    let props = lodestone_data::block_states::properties(state_id)?;
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
) -> Vec<([i32; 3], u32, SignText)> {
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
fn sign_spawn(block: [i32; 3], state_id: u32, text: SignText, light: u8) -> Option<SignSpawn> {
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
    out
}

/// The block's own dye colour, for a **standing** banner — `white_banner` →
/// `DyeColor::White` (issue #23).
///
/// The base colour is the *block*, not a state property: vanilla ships sixteen
/// separate banner blocks. Grepping for a `color` property here finds nothing and
/// draws every banner white, which is the natural mistake because shulker boxes
/// are spelled the same way and skulls are not.
///
/// Wall banners (`*_wall_banner`) return `None` deliberately: their body layer is
/// `createBodyLayer(false)`, a different mesh the asset corpus does not build, so
/// drawing one with the standing rig would hang a full pole in mid-air.
#[must_use]
fn standing_banner_colour(state_id: u32) -> Option<DyeColor> {
    let name = lodestone_data::block_states::block_name(state_id)?;
    let path = name.strip_prefix("minecraft:").unwrap_or(name);
    DyeColor::from_name(path.strip_suffix("_banner")?)
}

/// A banner's `rotation` state property, `0..16`. Vanilla's `RotationSegment`,
/// not a four-way `facing` — see `banner_ground_placement_matrix`.
#[must_use]
fn banner_rotation_segment(state_id: u32) -> Option<u8> {
    let props = lodestone_data::block_states::properties(state_id)?;
    props
        .iter()
        .find(|(name, _)| *name == "rotation")
        .and_then(|(_, value)| value.parse::<u8>().ok())
}

/// The block entity's stored pattern stack, parsed out of its NBT.
///
/// `BannerPatternLayers.Layer.CODEC` is `{pattern: <id>, color: <dye name>}` and
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

/// One candidate resolved into a [`BannerSpawn`], or `None` when the state is not
/// a standing banner.
#[must_use]
fn banner_spawn(
    block: [i32; 3],
    state_id: u32,
    patterns: Vec<StoredPatternLayer>,
    phase: f32,
    light: u8,
) -> Option<BannerSpawn> {
    Some(BannerSpawn {
        pos: block,
        rotation_segment: banner_rotation_segment(state_id)?,
        base_color: standing_banner_colour(state_id)?,
        patterns,
        phase,
        light,
    })
}

/// Every banner to draw this frame (issue #23).
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

    #[test]
    fn every_ported_skull_block_in_the_real_table_resolves() {
        for path in [
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
        ] {
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

    /// The two real skull types this renderer declines — dragon and piglin —
    /// must still be *present* in the state table (so this is testing the
    /// decline, not a stale block name) and must resolve to no skull type.
    #[test]
    fn declined_skull_types_are_present_but_resolve_to_nothing() {
        for path in ["dragon_head", "dragon_wall_head", "piglin_head", "piglin_wall_head"] {
            let name = format!("minecraft:{path}");
            let found = (0..lodestone_data::block_states::STATE_COUNT)
                .find(|id| lodestone_data::block_states::block_name(*id) == Some(name.as_str()));
            let id = found.unwrap_or_else(|| panic!("{name} is not in the 26.2 state table"));
            assert_eq!(
                skull_type_for_state(id),
                None,
                "{name} unexpectedly resolved a skull type"
            );
        }
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
        assert!(bell_is_present(id));
        let shakes = BellShakes::new();
        let spawn = bell_spawn([1, 2, 3], id, lodestone_render::ENTITY_FULLBRIGHT, &shakes, 0.0)
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
        let spawn = bell_spawn(pos, id, lodestone_render::ENTITY_FULLBRIGHT, &shakes, 0.0)
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

        // And it ends: `BellBlockEntity.tick` clears at 50.
        for _ in 0..45 {
            shakes.tick();
        }
        assert!(shakes.is_empty(), "the shake outlived its 50-tick window");
        assert_eq!(shakes.shake(pos, 0.0), None);
    }

    /// The four horizontal faces map to vanilla's own `from3DDataValue` order, and
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
            assert!(!bell_is_present(id), "{name} matched as a bell");
            assert_eq!(
                bell_spawn(
                    [0, 0, 0],
                    id,
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
                    assert!(sign_kind_for_state(id).is_some());
                    match sign_orientation(id).expect("a ground sign must have an orientation") {
                        SignOrientation::Ground { rotation_segment } => {
                            ground_segments.insert(rotation_segment);
                        }
                        SignOrientation::Wall { .. } => panic!("oak_sign resolved as wall"),
                    }
                }
                Some("minecraft:oak_wall_sign") => {
                    wall_states += 1;
                    assert!(sign_kind_for_state(id).is_some());
                    match sign_orientation(id).expect("a wall sign must have an orientation") {
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
                assert!(sign_kind_for_state(id).is_some(), "{name} (state {id}) not a sign");
                assert!(
                    sign_orientation(id).is_some(),
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
                sign_kind_for_state(id),
                Some(SignKind::Hanging),
                "{name} (state {id}) must resolve as a hanging sign"
            );
            assert!(
                sign_orientation(id).is_some(),
                "{name} (state {id}) resolved no orientation"
            );
        }
        for path in ["oak_sign", "oak_wall_sign"] {
            let name = format!("minecraft:{path}");
            let id = (0..lodestone_data::block_states::STATE_COUNT)
                .find(|id| lodestone_data::block_states::block_name(*id) == Some(name.as_str()))
                .unwrap_or_else(|| panic!("{name} is not in the 26.2 state table"));
            assert_eq!(sign_kind_for_state(id), Some(SignKind::Plain), "{name}");
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
                    assert_eq!(sign_kind_for_state(id), Some(SignKind::Hanging), "{name}");
                    match sign_orientation(id) {
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
                sign_kind_for_state(id).is_none(),
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
        text.front.lines[0] = "LODESTONE PROBE".to_owned();
        let spawn = sign_spawn([0, 64, 0], id, text, lodestone_render::ENTITY_FULLBRIGHT)
            .expect("a real oak_sign state must resolve to a spawn");
        assert_eq!(spawn.front.lines[0], "LODESTONE PROBE");
        assert!(matches!(spawn.orientation, SignOrientation::Ground { .. }));
    }

    #[test]
    fn sign_spawns_before_login_is_empty_rather_than_a_panic() {
        let handle: SharedHandle = std::sync::Arc::new(std::sync::OnceLock::new());
        assert!(sign_spawns(&handle, Vec3::ZERO).is_empty());
    }
}

/// Shulker boxes (issue #23) — kept in its own module beside `bell_tests` for the
/// same reason: this file is shared across every block-entity family.
#[cfg(test)]
mod shulker_tests {
    use super::*;

    /// Finds the first state id whose block name matches, against the real 26.2
    /// table rather than a fixture.
    fn state_named(name: &str) -> u32 {
        (0..lodestone_data::block_states::STATE_COUNT)
            .find(|id| lodestone_data::block_states::block_name(*id) == Some(name))
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
            let (_, facing) = shulker_orientation(id).expect("resolves");
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
}
