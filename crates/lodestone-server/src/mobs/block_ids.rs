//! Block-state id lookups shared by [`super::ChunkWorld`] and by the wider
//! crate through the [`block_state_id`]/[`block_state_id_or_default`] doors —
//! moved out of `mobs/mod.rs` verbatim as part of the `mobs.rs` file split
//! (see `docs/plans/crate-and-file-splits.md`). No `MobSim` dependency: this
//! is the pure block-state-id half of the terrain adapter.

use std::collections::HashMap;
use std::sync::OnceLock;

use lodestone_data::block_states;
use lodestone_entity::pathfinding::PathType;
use lodestone_model::PathType as CensusPathType;

/// Translates `lodestone_model::PathType` (what the census in
/// [`lodestone_data::path_types`] is keyed by) into
/// `lodestone_entity::pathfinding::PathType` (what the A* search and malus
/// table consume). The two enums are deliberately separate crates on
/// opposite sides of the version seam — see `pathfinding/mod.rs`'s own doc
/// ("a real adapter... maps real block-state ids to `PathType`") — so this is
/// the translation layer that doc promises, not a rename. Every variant is
/// named on both sides identically; the match is exhaustive on the census
/// side so a future variant added to either enum fails to compile here
/// instead of silently falling through.
pub(super) fn census_to_pathfinding_type(pt: CensusPathType) -> PathType {
    match pt {
        CensusPathType::Blocked => PathType::Blocked,
        CensusPathType::Open => PathType::Open,
        CensusPathType::Walkable => PathType::Walkable,
        CensusPathType::WalkableDoor => PathType::WalkableDoor,
        CensusPathType::Trapdoor => PathType::Trapdoor,
        CensusPathType::PowderSnow => PathType::PowderSnow,
        CensusPathType::OnTopOfPowderSnow => PathType::OnTopOfPowderSnow,
        CensusPathType::Fence => PathType::Fence,
        CensusPathType::Lava => PathType::Lava,
        CensusPathType::Water => PathType::Water,
        CensusPathType::WaterBorder => PathType::WaterBorder,
        CensusPathType::Rail => PathType::Rail,
        CensusPathType::UnpassableRail => PathType::UnpassableRail,
        CensusPathType::FireInNeighbor => PathType::FireInNeighbor,
        CensusPathType::Fire => PathType::Fire,
        CensusPathType::DamagingInNeighbor => PathType::DamagingInNeighbor,
        CensusPathType::Damaging => PathType::Damaging,
        CensusPathType::DoorOpen => PathType::DoorOpen,
        CensusPathType::DoorWoodClosed => PathType::DoorWoodClosed,
        CensusPathType::DoorIronClosed => PathType::DoorIronClosed,
        CensusPathType::Breach => PathType::Breach,
        CensusPathType::Leaves => PathType::Leaves,
        CensusPathType::StickyHoney => PathType::StickyHoney,
        CensusPathType::Cocoa => PathType::Cocoa,
        CensusPathType::DamageCautious => PathType::DamageCautious,
        CensusPathType::OnTopOfTrapdoor => PathType::OnTopOfTrapdoor,
        CensusPathType::BigMobsCloseToDanger => PathType::BigMobsCloseToDanger,
    }
}

/// Renders block-state `id`'s canonical string (`"minecraft:name"`, or
/// `"minecraft:name[k=v,k2=v2]"` with properties sorted by key) — the exact
/// format [`ChunkColumn::block_state`] stores, so the two agree without
/// either side special-casing the other. Mirrors
/// `lodestone_worldgen::surface::block_json_key`'s key format, which is where
/// that format is proven to match vanilla's own `BlockState.CODEC`
/// canonicalisation (see that function's doc comment).
fn canonical_state_string(id: u32) -> Option<String> {
    let name = block_states::block_name(id)?;
    let props = block_states::properties(id)?;
    if props.is_empty() {
        return Some(name.to_string());
    }
    let mut s = String::with_capacity(name.len() + 2);
    s.push_str(name);
    s.push('[');
    for (i, (k, v)) in props.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(k);
        s.push('=');
        s.push_str(v);
    }
    s.push(']');
    Some(s)
}

/// The reverse of [`canonical_state_string`]: every block-state id's canonical
/// string, keyed back to its id. Built once (32,366 entries) and cached for
/// the process lifetime, because `ChunkColumn` stores block states as canonical
/// strings rather than ids while every per-state census is keyed by id, so
/// `ChunkWorld` has to bridge the two.
///
/// **`lodestone-data` now has a forward index and this map should go.** This
/// doc used to say that crate had "no name → id lookup"; that stopped being
/// true when `lodestone_data::block_states::state_id` landed — a block-major
/// sorted-span lookup that needs no per-process map and no 32,366 `String`
/// allocations. It is not merely equivalent: it resolves a *partial* property
/// set through vanilla's `defaultBlockState().setValue(…)` semantics, which
/// this exact-string map cannot, and that difference was three shipped bugs.
/// Do not add a fourth hand-rolled inverse anywhere — call that instead.
pub(super) fn state_id_by_name() -> &'static HashMap<String, u32> {
    static INDEX: OnceLock<HashMap<String, u32>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut map = HashMap::with_capacity(block_states::STATE_COUNT as usize);
        for id in 0..block_states::STATE_COUNT {
            if let Some(key) = canonical_state_string(id) {
                map.insert(key, id);
            }
        }
        map
    })
}

/// The global block-state id for a canonical state string, or `None` for a name
/// this version's census does not carry.
///
/// The one public door onto [`state_id_by_name`]'s cached index, added for
/// [`crate::block_drops`]'s correct-tool gate (issue #539): every per-block-state
/// census in `lodestone-data` — hardness, tool rules, collision — is keyed by
/// **id**, while `ChunkColumn` stores states as canonical **strings**, so
/// anything that reads one of those censuses for a block the world names has to
/// cross this bridge. Kept here rather than duplicated because building the
/// 32,366-entry map twice per process would be the only alternative.
///
/// Accepts a bare name (`"minecraft:stone"`) or one with properties
/// (`"minecraft:oak_log[axis=y]"`), since that is exactly what
/// [`ChunkColumn::block_state`] returns.
#[must_use]
pub(crate) fn block_state_id(name: &str) -> Option<u32> {
    state_id_by_name().get(name).copied()
}

/// The **lowest** block-state id belonging to a bare block name — a stand-in for
/// vanilla's `Block.defaultBlockState()` for callers that only need a
/// per-*block* census row.
///
/// Built once (1,196 entries) beside [`state_id_by_name`] and cached the same
/// way. Ids are allocated contiguously per block, so the lowest id of a block is
/// one of its states; every census that keys on a state id but whose value is a
/// property of the *block* — [`lodestone_data::hardness`] and
/// [`lodestone_data::tool`], the two [`crate::block_breaking`] reads — gives the
/// same answer for any of them. It is **not** a substitute for
/// [`block_state_id`] where the properties matter (collision shapes, path
/// types); those must resolve the exact state.
#[must_use]
fn default_state_id_by_block() -> &'static HashMap<&'static str, u32> {
    static INDEX: OnceLock<HashMap<&'static str, u32>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut map: HashMap<&'static str, u32> =
            HashMap::with_capacity(block_states::BLOCK_COUNT as usize);
        for id in 0..block_states::STATE_COUNT {
            if let Some(name) = block_states::block_name(id) {
                map.entry(name).or_insert(id);
            }
        }
        map
    })
}

/// [`block_state_id`], falling back to the block's default state when the exact
/// state string is not in the index.
///
/// The fallback exists because a *bare* name is only in [`state_id_by_name`] for
/// a block with **no properties**: `"minecraft:stone"` resolves, and
/// `"minecraft:sugar_cane"` does not, because every sugar cane state carries
/// `age`. Anything that names a block without spelling out its properties — a
/// feature's simple state provider, a test fixture, a `/setblock`-shaped string —
/// therefore misses, and [`crate::block_breaking`] read that miss as "unknown
/// block, do not validate", which is exactly the one-shot-block bug it was
/// written to fix.
///
/// Only use this where the census being read is per-*block* rather than
/// per-state; see [`default_state_id_by_block`].
#[must_use]
pub(crate) fn block_state_id_or_default(name: &str) -> Option<u32> {
    if let Some(id) = block_state_id(name) {
        return Some(id);
    }
    let base = name.split('[').next()?;
    default_state_id_by_block().get(base).copied()
}
