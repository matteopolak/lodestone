//! Comparing a plan's witnessed cells against the live world, to notice
//! terrain that changed after the plan was committed (`docs/baritone-port.md`
//! §4.5).
//!
//! # Why this samples rather than subscribes
//!
//! §4.5's own design assumes a `SectionBlocksChanged`/`BlockChangedAck` event
//! stream a plan's witness set can be tested against, `O(block updates)` per
//! tick. **This client has no such event reaching a plugin** —
//! `lodestone-ecs` emits nothing per-block-change today, and adding that is
//! outside this crate's ownership (`docs/autonomous-navigation.md` records
//! the gap rather than inventing an event bus this crate should not own). So
//! this module samples the live world directly and diffs against a baseline
//! taken at commit time — `O(cells checked)` per check, the same asymptotic
//! shape as the design's own cost analysis, just paid on a caller-chosen
//! cadence instead of being event-driven. `lodestone_autopilot::plan_route`
//! is that caller: a cheap look-ahead-window check every tick, plus a
//! rate-limited full sweep for the part of the plan not yet in the window.
//!
//! One cell is a single `HashMap` lookup keyed by chunk
//! ([`lodestone_world::World::get`]) — no [`crate::view::SnapshotView`] is
//! built, because that type exists to answer thousands of queries against one
//! frozen snapshot and would waste an `Arc` clone per section of a
//! `(2r+1)²` grid for what is, here, a handful of point reads.
//!
//! Only raw block-state ids are compared, never re-derived legality. That
//! matches §4.5's own letter ("a hit marks the plan stale") rather than its
//! event source: any change at a witnessed cell is treated as suspect,
//! whether or not the new block would actually still be legal there — cheap,
//! conservative, and it needs no [`crate::facts::FactsTable`] at all.

use std::collections::{HashMap, HashSet};

use lodestone_world::{ChunkPos, World};

use crate::graph::NavNode;

/// The block-state id at `(x, y, z)`, or `None` when that column is not
/// loaded — the same "outside the snapshot" answer [`crate::view::NavView`]
/// gives, and for the same reason: an unloaded column is not evidence the
/// plan is still valid, it is evidence there is nothing to check yet.
#[must_use]
pub fn point_state(world: &World, x: i32, y: i32, z: i32) -> Option<u32> {
    let pos = ChunkPos::new(x.div_euclid(16), z.div_euclid(16));
    let chunk = world.get(pos)?;
    #[allow(clippy::cast_sign_loss)]
    Some(chunk.column.get_block(
        x.rem_euclid(16) as usize,
        y,
        z.rem_euclid(16) as usize,
    ))
}

/// Snapshot every cell in `witnesses` that currently resolves, keyed the same
/// way [`crate::plan::Plan::witnesses`] packs them.
///
/// Unresolved cells (column not loaded yet) are simply omitted: a plan's own
/// witnesses are cells its edges actually read while a search view had them
/// loaded, so at commit time — called immediately after the plan is
/// adopted — they resolve, unless the world already changed under the
/// search, which [`first_change`] would then also catch on the very next
/// call.
#[must_use]
pub fn sample(world: &World, witnesses: &HashSet<u64>) -> HashMap<u64, u32> {
    witnesses
        .iter()
        .filter_map(|&key| {
            let node = NavNode::unpack(key)?;
            let state = point_state(world, node.x, node.y, node.z)?;
            Some((key, state))
        })
        .collect()
}

/// The first witnessed cell whose live state no longer matches `baseline`,
/// unpacked back to a world position for diagnostics.
///
/// A cell that no longer resolves at all — its column unloaded since the
/// baseline was taken — counts as changed too: conservative, and the same
/// "chunk went away under the plan" case §4.5 names, folded into the same
/// replan trigger rather than a separate truncate path
/// (`docs/autonomous-navigation.md` records the simplification).
#[must_use]
pub fn first_change(world: &World, baseline: &HashMap<u64, u32>) -> Option<(i32, i32, i32)> {
    baseline.iter().find_map(|(&key, &recorded)| {
        let node = NavNode::unpack(key)?;
        let live = point_state(world, node.x, node.y, node.z);
        (live != Some(recorded)).then_some((node.x, node.y, node.z))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_world::{ChunkColumn, ColumnLight, Heightmaps, LoadedChunk, PaletteKind};

    const AIR: u32 = 0;
    const STONE: u32 = 1;

    fn flat_world() -> World {
        let mut world = World::new();
        let block_kind = PaletteKind::block_states();
        let biome_kind = PaletteKind::biomes();
        let mut column = ChunkColumn::new(0, 4, block_kind, biome_kind, AIR, 0);
        for lx in 0..16usize {
            for lz in 0..16usize {
                column.set_block(lx, 0, lz, STONE);
            }
        }
        let light = ColumnLight::new(4);
        world.load(
            ChunkPos::new(0, 0),
            LoadedChunk::new(column, light, Heightmaps::default(), Vec::new()),
        );
        world
    }

    #[test]
    fn point_state_reads_a_loaded_cell_and_none_outside_it() {
        let world = flat_world();
        assert_eq!(point_state(&world, 3, 0, 5), Some(STONE));
        assert_eq!(point_state(&world, 3, 1, 5), Some(AIR));
        assert_eq!(point_state(&world, 200, 0, 5), None, "column not loaded");
    }

    #[test]
    fn a_baseline_with_no_changes_reports_none() {
        let world = flat_world();
        let mut witnesses = HashSet::new();
        witnesses.insert(NavNode::still(3, 0, 5).try_pack().unwrap());
        witnesses.insert(NavNode::still(4, 0, 5).try_pack().unwrap());
        let baseline = sample(&world, &witnesses);
        assert_eq!(baseline.len(), 2);
        assert_eq!(first_change(&world, &baseline), None);
    }

    /// The load-bearing case: a block actually changing under a committed
    /// plan must be caught, and reported at the position it happened.
    #[test]
    fn a_changed_cell_is_reported_by_position() {
        let mut world = flat_world();
        let mut witnesses = HashSet::new();
        witnesses.insert(NavNode::still(3, 0, 5).try_pack().unwrap());
        let baseline = sample(&world, &witnesses);
        world.set_block(3, 0, 5, AIR); // someone broke the block under the plan
        assert_eq!(first_change(&world, &baseline), Some((3, 0, 5)));
    }

    /// The conservative fold-in from the module docs: a witnessed cell whose
    /// column unloads counts as changed too, not as "cannot tell".
    #[test]
    fn a_chunk_that_unloads_under_the_plan_counts_as_changed() {
        let world = flat_world();
        let mut witnesses = HashSet::new();
        witnesses.insert(NavNode::still(3, 0, 5).try_pack().unwrap());
        let baseline = sample(&world, &witnesses);
        let mut world = world;
        world.unload(ChunkPos::new(0, 0));
        assert_eq!(first_change(&world, &baseline), Some((3, 0, 5)));
    }

    /// The unreachable control: a baseline sampled against a witness set that
    /// never resolved at all (outside the loaded column) has nothing to
    /// compare and must never fire a false positive.
    #[test]
    fn an_unresolved_witness_never_produces_a_false_change() {
        let world = flat_world();
        let mut witnesses = HashSet::new();
        witnesses.insert(NavNode::still(500, 0, 500).try_pack().unwrap());
        let baseline = sample(&world, &witnesses);
        assert!(baseline.is_empty(), "an unloaded cell must not enter the baseline at all");
        assert_eq!(first_change(&world, &baseline), None);
    }
}
