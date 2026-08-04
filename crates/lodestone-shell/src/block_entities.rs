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
use lodestone_render::{ChestHalf, ChestMaterial, ChestSpawn, horizontal_facing_yaw};
use lodestone_world::{ChunkPos, World};

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
