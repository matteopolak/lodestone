//! **The boundary-write control for Unit 7's in-place region decoration.**
//!
//! # What it is
//!
//! One assertion, at the production serve boundary: a tree whose trunk stands in
//! one chunk must have canopy in the chunk next door, and that canopy must be
//! present **on both sides of the seam** in the bytes a client is actually served.
//!
//! # Why this file exists at all
//!
//! Unit 7 (`docs/plans/worldgen-rewrite.md`) rewrote the coordinate space that
//! decoration reads and writes through — from a stitched `48 × height × 48` copy of
//! the 3×3 neighbourhood to [`lodestone_worldgen::feature::region_view`]'s routed
//! view over the nine source grids. That is the exact coordinate space of the
//! repo's worst recorded worldgen defect: `VegGrid` once stored and exposed local
//! coordinates while the placement engine handed it absolute `BlockPos`es, so every
//! write outside chunk `(0, 0)` failed an implicit bounds test and **vegetation
//! reached zero blocks in every served chunk with the whole unit suite green**.
//! Worse, the sweep gate that caught it was later deleted while a doc comment went
//! on naming it, so for an unknown span the repo held a written record of a
//! regression with nothing watching for its return — and the reference read as
//! coverage on inspection.
//!
//! A count of vegetation is not enough to catch the *seam* half of that. Vanilla's
//! `blockStateWriteRadius(1)` at the FEATURES stage is why the 3×3 driver exists:
//! a chunk's served content legitimately includes blocks placed by its neighbours'
//! own decoration passes. A view that silently clipped writes at the chunk border
//! would still produce plenty of vegetation — just subtly wrong at every chunk
//! edge, which is the failure mode a total is blind to.
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
//! The two hypotheses are far apart, which is the point: with the seam handled, the
//! measurement is **20 crossings and 24 orphan leaves**. With the routing broken —
//! `source_slot` using truncating `/ 16` instead of `div_euclid(16)`, which maps the
//! western third of the region onto the centre — both go to **0**, and that was
//! *observed*, not described (2026-08-07, the break reverted from a scratchpad
//! backup with an md5 check).
//!
//! # One thing the first attempt at this control taught, worth keeping
//!
//! The obvious way to "break the seam" — narrowing `VegGrid::in_bounds_local` from
//! the padded region down to `0..16`, the historical bug's own shape — left this
//! control **passing, with byte-identical counts**. That is not a weakness in the
//! control; it is a fact about the pipeline that was not obvious beforehand. When
//! chunk `E` is the centre, its western neighbour's pass runs against `E`'s own grid
//! origin, so that neighbour's spilled canopy already lands at local `x = 0, 1` —
//! *inside* `0..16` — and the fold-back drops everything outside the centre anyway.
//! So the padded footprint (`VEG_PADDING`) governs **intra-pass reads and the
//! census**, not which blocks reach the wire. A control aimed at the fold-back
//! bound would therefore be premise-false: it would fire for a reason unrelated to
//! what it claims to measure. The serve-visible seam mechanism is the 3×3 driver
//! plus the view's coordinate routing, which is what the break above attacks.
//!
//! # How to change it
//!
//! If a later unit legitimately changes vegetation (U8's placement-engine port,
//! U11's 3-D biomes) these floors may move. **Re-measure and lower/raise the floor
//! with the new number recorded here — do not delete the test**, which is precisely
//! what happened to its predecessor. If the swamp at `(-9, 18)` stops being a
//! swamp, pick a new pair by re-running the same two functions over a lattice and
//! record why.
//!
//! # Dependencies
//!
//! The bundled embedded worldgen data via
//! [`lodestone_server::overworld_generator`]; no oracle, no fixture file.

use lodestone_worldgen::overworld::GeneratedColumn;

/// Seed and chunk pair, fixed. Both chunks are `minecraft:swamp` at seed 42.
const SEED: i64 = 42;
const WEST: (i32, i32) = (-9, 18);
const EAST: (i32, i32) = (-8, 18);

/// Measured at the coordinates above with the seam handled correctly. The
/// competing hypothesis — writes clipped at the chunk border — is **0** for both,
/// so the floors sit well below the measurement and well above the defect.
const MEASURED_CROSSINGS: usize = 20;
const MEASURED_ORPHANS: usize = 24;
const CROSSING_FLOOR: usize = 12;
const ORPHAN_FLOOR: usize = 12;

/// How far a real canopy can sit from its own trunk. 8 columns is past the reach
/// of every 26.2 overworld tree (the widest, a 2×2 dark oak, spans ~3 from its
/// trunk), so a leaf with no log inside this window has no trunk in this chunk.
const TRUNK_REACH: i32 = 8;

fn is_leaf(state: &str) -> bool {
    state.contains("_leaves")
}

fn is_log(state: &str) -> bool {
    state.contains("_log") || state.contains("_wood") || state.contains("_stem")
}

/// `(y, lz)` positions where one canopy spans the seam: tree material at the west
/// chunk's two easternmost columns and leaves at the east chunk's two westernmost,
/// all in the same row.
fn contiguous_crossings(west: &GeneratedColumn, east: &GeneratedColumn) -> Vec<(i32, i32)> {
    let mut out = Vec::new();
    for y in west.min_y()..west.min_y() + west.height() {
        for lz in 0..16usize {
            let w14 = west.block_state(14, y, lz);
            let w15 = west.block_state(15, y, lz);
            let e0 = east.block_state(0, y, lz);
            let e1 = east.block_state(1, y, lz);
            if (is_leaf(w14) || is_log(w14))
                && (is_leaf(w15) || is_log(w15))
                && is_leaf(e0)
                && is_leaf(e1)
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
            if !is_leaf(col.block_state(0, y, lz as usize)) {
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
                        if is_log(col.block_state(tx as usize, ty, tz as usize)) {
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
        crossings.len() >= CROSSING_FLOOR,
        "vanilla's blockStateWriteRadius(1) spill is missing at the {WEST:?}|{EAST:?} \
         seam: {} rows carry one canopy across it, floor {CROSSING_FLOOR} \
         (measured {MEASURED_CROSSINGS} with the seam handled; the clipped-write \
         hypothesis is 0). bbox(y,z) = {:?}",
        crossings.len(),
        bbox(&crossings),
    );

    // --- Signature 2: those blocks cannot be from a local tree -------------
    let orphans = orphan_west_leaves(&east);
    assert!(
        orphans.len() >= ORPHAN_FLOOR,
        "chunk {EAST:?} carries {} leaf cells on its west edge with no trunk within \
         {TRUNK_REACH} columns, floor {ORPHAN_FLOOR} (measured {MEASURED_ORPHANS}; the \
         clipped-write hypothesis is 0). Without these, signature 1 could be two \
         unrelated adjacent trees. bbox(y,z) = {:?}",
        orphans.len(),
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

fn count(col: &GeneratedColumn, pred: fn(&str) -> bool) -> usize {
    let mut n = 0;
    for y in col.min_y()..col.min_y() + col.height() {
        for lz in 0..16usize {
            for lx in 0..16usize {
                if pred(col.block_state(lx, y, lz)) {
                    n += 1;
                }
            }
        }
    }
    n
}
