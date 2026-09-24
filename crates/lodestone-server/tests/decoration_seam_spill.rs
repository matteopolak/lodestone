//! Production boundary control for vegetation spill across adjacent chunks.
//!
//! # What it is
//!
//! A canopy crossing a chunk border must be present on both sides in the served
//! columns. The detector uses locations, not a generated total.
//!
//! # How it works
//!
//! Fixed seed and fixed coordinates, chosen by measurement (see the probe numbers
//! in the test below) rather than by hope: chunk `(-9, 18)` and its eastern
//! neighbour `(-8, 18)` are swamp, and a canopy there straddles their shared
//! border. Two independent signatures, both computed from the served
//! [`GeneratedColumn`]s:
//!
//! * **[`contiguous_crossings`]** — tree material at the west chunk's `lx = 14` and
//!   `lx = 15` *and* leaves at the east chunk's `lx = 0` and `lx = 1`, all at the
//!   same `(y, z)`. A four-block-wide band of one canopy spanning the seam. This is
//!   the "present on both sides" claim, literally.
//! * **[`orphan_west_leaves`]** — leaves on the east chunk's `lx = 0` with **no log
//!   anywhere** within 8 columns east, ±8 in z, and 12 blocks below. Those leaves
//!   cannot belong to a tree in their own chunk, so they arrived across the seam.
//!   This is what rules out "two unrelated trees happened to be adjacent".
//!
//! The production assertion is location-based rather than a fixed generated total.
//! A companion test supplies a deliberately broken route and requires the detector
//! to lose the crossing signature.

use lodestone_data::block_states::StateId;
use lodestone_worldgen::overworld::GeneratedColumn;

/// Seed and chunk pair, fixed. Both chunks are `minecraft:swamp` at seed 42.
const SEED: i64 = 42;
const WEST: (i32, i32) = (-9, 18);
const EAST: (i32, i32) = (-8, 18);

/// How far a real canopy can sit from its own trunk. 8 columns is past the reach
/// of every 26.2 overworld tree (the widest, a 2×2 dark oak, spans ~3 from its
/// trunk), so a leaf with no log inside this window has no trunk in this chunk.
const TRUNK_REACH: i32 = 8;

fn is_leaf(state: StateId) -> bool {
    state.name().contains("_leaves")
}

fn is_log(state: StateId) -> bool {
    let name = state.name();
    name.contains("_log") || name.contains("_wood") || name.contains("_stem")
}

/// `(y, lz)` positions where one canopy spans the seam: tree material at the west
/// chunk's two easternmost columns and leaves at the east chunk's two westernmost,
/// all in the same row.
fn contiguous_crossings(west: &GeneratedColumn, east: &GeneratedColumn) -> Vec<(i32, i32)> {
    contiguous_crossings_routed(
        |x, y, z| {
            if x < 16 {
                is_leaf(west.block_state_id(x as usize, y, z as usize))
                    || is_log(west.block_state_id(x as usize, y, z as usize))
            } else {
                is_leaf(east.block_state_id((x - 16) as usize, y, z as usize))
                    || is_log(east.block_state_id((x - 16) as usize, y, z as usize))
            }
        },
        |x, y, z| {
            if x < 16 {
                is_leaf(west.block_state_id(x as usize, y, z as usize))
            } else {
                is_leaf(east.block_state_id((x - 16) as usize, y, z as usize))
            }
        },
        west.min_y(),
        west.height(),
    )
}

fn contiguous_crossings_routed<T, L>(
    mut is_tree_at: T,
    mut is_leaf_at: L,
    min_y: i32,
    height: i32,
) -> Vec<(i32, i32)>
where
    T: FnMut(i32, i32, i32) -> bool,
    L: FnMut(i32, i32, i32) -> bool,
{
    let mut out = Vec::new();
    for y in min_y..min_y + height {
        for lz in 0..16usize {
            if is_tree_at(14, y, lz as i32)
                && is_tree_at(15, y, lz as i32)
                && is_leaf_at(16, y, lz as i32)
                && is_leaf_at(17, y, lz as i32)
            {
                out.push((y, lz as i32));
            }
        }
    }
    out
}

/// `(y, lz)` positions where `col` carries a leaf on its western edge whose trunk
/// is nowhere in `col` — a canopy that arrived from the chunk to the west.
fn orphan_west_leaves(col: &GeneratedColumn) -> Vec<(i32, i32)> {
    let mut out = Vec::new();
    for y in col.min_y()..col.min_y() + col.height() {
        for lz in 0..16i32 {
            if !is_leaf(col.block_state_id(0, y, lz as usize)) {
                continue;
            }
            let mut trunk_in_this_chunk = false;
            for tx in 0..=TRUNK_REACH.min(15) {
                for dz in -TRUNK_REACH..=TRUNK_REACH {
                    let tz = lz + dz;
                    if !(0..16).contains(&tz) {
                        continue;
                    }
                    for ty in (y - 12).max(col.min_y())..=y {
                        if is_log(col.block_state_id(tx as usize, ty, tz as usize)) {
                            trunk_in_this_chunk = true;
                        }
                    }
                }
            }
            if !trunk_in_this_chunk {
                out.push((y, lz));
            }
        }
    }
    out
}

/// `(y_min, y_max, z_min, z_max)` of a position list, so a failure names *where*
/// rather than only *how much* — CLAUDE.md's "measure by location, never by frame
/// average", which has diagnosed two premise-false controls in this repo already.
fn bbox(positions: &[(i32, i32)]) -> Option<(i32, i32, i32, i32)> {
    let mut it = positions.iter();
    let &(y0, z0) = it.next()?;
    let mut b = (y0, y0, z0, z0);
    for &(y, z) in it {
        b.0 = b.0.min(y);
        b.1 = b.1.max(y);
        b.2 = b.2.min(z);
        b.3 = b.3.max(z);
    }
    Some(b)
}

#[test]
fn a_canopy_spans_the_chunk_seam_in_both_served_chunks() {
    let generator = lodestone_server::overworld_generator(SEED);
    let west = generator.column(WEST.0, WEST.1);
    let east = generator.column(EAST.0, EAST.1);

    // --- Premise checks, before believing any number below -----------------
    // "Before believing a control, ask what else already paints here." If this
    // pair stopped being forested the crossing count would fall to zero for a
    // reason that has nothing to do with the seam, and the test would report a
    // seam defect that does not exist.
    let leaves_west = count(&west, is_leaf);
    let leaves_east = count(&east, is_leaf);
    let logs_west = count(&west, is_log);
    assert!(
        logs_west > 0 && leaves_west > 0 && leaves_east > 0,
        "premise failed: chunk {WEST:?} must actually carry trees for a seam crossing \
         to be possible at all (logs_west={logs_west}, leaves_west={leaves_west}, \
         leaves_east={leaves_east}). Biomes are {} / {} — if these are no longer \
         forested, choose a new pair rather than weakening the assertions.",
        west.biome_state(8, 8),
        east.biome_state(8, 8),
    );

    // --- Signature 1: present on both sides -------------------------------
    let crossings = contiguous_crossings(&west, &east);
    assert!(
        !crossings.is_empty(),
        "no contiguous canopy crosses the {WEST:?}|{EAST:?} seam; bbox(y,z) = {:?}",
        bbox(&crossings),
    );

    // --- Signature 2: those blocks cannot be from a local tree -------------
    let orphans = orphan_west_leaves(&east);
    assert!(
        !orphans.is_empty(),
        "chunk {EAST:?} has no west-edge leaves without a local trunk; bbox(y,z) = {:?}",
        bbox(&orphans),
    );

    // The two signatures must agree about *where*, not merely both be non-zero:
    // a crossing row that no orphan leaf shares would mean the two are measuring
    // different trees.
    let crossing_rows: std::collections::HashSet<(i32, i32)> = crossings.iter().copied().collect();
    let shared = orphans.iter().filter(|p| crossing_rows.contains(p)).count();
    assert!(
        shared > 0,
        "the two signatures do not overlap: {} crossings at {:?} vs {} orphan leaves at \
         {:?}. They must describe the same canopy, or one of them is measuring \
         something else.",
        crossings.len(),
        bbox(&crossings),
        orphans.len(),
        bbox(&orphans),
    );

    println!(
        "seam {WEST:?}|{EAST:?}: crossings={} orphan_west_leaves={} shared={} bbox={:?}",
        crossings.len(),
        orphans.len(),
        shared,
        bbox(&crossings),
    );
}

#[test]
fn routing_the_east_half_to_air_is_detected() {
    let generator = lodestone_server::overworld_generator(SEED);
    let west = generator.column(WEST.0, WEST.1);
    let east = generator.column(EAST.0, EAST.1);
    let intact = contiguous_crossings(&west, &east);
    assert!(!intact.is_empty(), "control premise: the fixture must cross the seam");

    let broken = contiguous_crossings_routed(
        |x, y, z| {
            x < 16 && {
                let state = west.block_state_id(x as usize, y, z as usize);
                is_leaf(state) || is_log(state)
            }
        },
        |x, y, z| {
            x < 16 && is_leaf(west.block_state_id(x as usize, y, z as usize))
        },
        west.min_y(),
        west.height(),
    );
    assert!(
        broken.is_empty(),
        "broken east-half routing still reported {:?}; detector must fail when the \
         neighbour's side is replaced by air",
        bbox(&broken),
    );
}

fn count(col: &GeneratedColumn, pred: fn(StateId) -> bool) -> usize {
    let mut n = 0;
    for y in col.min_y()..col.min_y() + col.height() {
        for lz in 0..16usize {
            for lx in 0..16usize {
                if pred(col.block_state_id(lx, y, lz)) {
                    n += 1;
                }
            }
        }
    }
    n
}
