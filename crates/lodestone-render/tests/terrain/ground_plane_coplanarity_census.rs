//! Census of **horizontal quads that sit on, or within a hair of, a block
//! boundary plane** — the population that can z-fight against the face of the
//! block above or below it — plus a gate on the ground-plate family's real
//! offsets.
//!
//! # Why this exists
//!
//! The owner reported "some blocks are popping in and out weirdly, like
//! z-fighting-ish — for example, the leaves on the ground". The family that
//! report names — flat, ground-hugging blocks: leaf litter, carpets, rails,
//! lily pads, redstone dust, pressure plates, snow layers — is not a list
//! anyone can write from memory correctly, and the property that decides
//! whether the depth buffer can separate such a plate from the ground is not
//! "is this block flat" but **how far its horizontal quads sit above the block
//! floor**. This scans every one of 26.2's block states through the **real
//! bake** ([`BlockModels::build`] over a real `client.jar`) and reports that
//! distance, per block.
//!
//! # What the numbers mean
//!
//! Two regimes, and only the first is unresolvable in principle:
//!
//! * **Exactly `0.0` from a boundary, with no `cullface`** — a quad the depth
//!   buffer can never separate from the neighbouring block's face at any
//!   distance under any projection. Which one wins is then decided purely by
//!   **draw order**. `lodestone-shell`'s `ground_plate_z_fight_pixels` gate
//!   measures that directly: with the plate snapped onto the boundary, two
//!   independent uploads of one unchanged world disagree on **2.7–5.4%** of
//!   the frame. This census prints that population (57 blocks, 4,441 quads in
//!   26.2 — panes, bars, fence-gate post tops, lightning rods, anvils) as
//!   information, because it is vanilla's own geometry and not ours to change.
//! * **A small non-zero offset** — leaf litter's `0.25/16`, a carpet's
//!   `1/16`, a snow layer's `2/16`. Resolvable up to some distance and not
//!   beyond. Measured for `0.015625` blocks through the **forward** `[0,1]`
//!   projection this renderer used to carry (`near = 0.05`, `Depth32Float`):
//!   **52 ULPs at 16 blocks, 13 at 32, 3 at 64**, and a grazing view makes it
//!   *better*, not worse, because the separation along the ray grows as `1/sin`
//!   while the distance grows only linearly. So the family's real offsets were
//!   resolvable throughout the render distance even then, and depth precision
//!   is not the mechanism behind the report. `Camera::projection_matrix` is
//!   reversed-Z now, which only widens that margin — see
//!   `docs/ground-plate-rendering.md`.
//!
//! A quad carrying a `cullface` is reported separately: it is only drawn when
//! the neighbour does not occlude, so it is a much weaker candidate.
//!
//! `#[ignore]`d and fail-closed: a missing jar is a loud panic, never a skip —
//! a census that silently scans nothing reports a clean bill of health.
//!
//! ```text
//! cargo test -p lodestone-render --test ground_plane_coplanarity_census -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;

use lodestone_assets::{ResourceManager, ZipSource};
use lodestone_data::block_states::StateId;
use lodestone_model::BlockStateRegistry;
use lodestone_render::{BlockModels, blocks_json_registry};

#[path = "../gate_harness/mod.rs"]
mod gate_harness;
use gate_harness::{require_blocks_report, require_client_jar};

/// A quad counts as horizontal when all four corners share a `y` within this.
/// Well below any real model offset (the smallest in 26.2 is `0.25/16`), so it
/// separates "flat plate" from "sloped or vertical" without judgement.
const PLANAR_EPS: f32 = 1e-5;

/// Only quads this close to a boundary are interesting; a slab's top face at
/// `0.5` cannot fight anything.
const NEAR_BOUNDARY: f32 = 0.1;

fn build_models() -> (BlockModels, Box<dyn BlockStateRegistry>) {
    let jar = require_client_jar();
    let report = require_blocks_report(&jar);
    let source = ZipSource::open(&jar).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(source)]);
    let registry = blocks_json_registry(&report).expect("parse blocks.json into a registry");
    let models = BlockModels::build(&manager, &registry).expect("bake block models");
    (models, Box::new(registry))
}

fn state_id(raw: u32) -> StateId {
    StateId::new(raw).expect("state id from the canonical blocks report")
}

/// `Some(y)` when the quad's four corners all share one `y`.
fn horizontal_plane(positions: &[[f32; 3]; 4]) -> Option<f32> {
    let y = positions[0][1];
    positions
        .iter()
        .all(|p| (p[1] - y).abs() <= PLANAR_EPS)
        .then_some(y)
}

/// Distance from `y` to the nearer of the two block boundaries it lies between.
fn distance_to_boundary(y: f32) -> f32 {
    (y - y.round()).abs()
}

/// One row of the census: `(block, distance-as-text, culled)` to
/// `(quad count, an example plane)`.
type Rows = BTreeMap<(String, String, bool), (usize, String)>;

fn census(models: &BlockModels, reg: &dyn BlockStateRegistry) -> (Rows, usize, usize) {
    // Keyed per block rather than per state: 26.2 has 32k states and a carpet
    // has one geometry per colour. The distance is part of the key so a block
    // whose quads sit at two heights shows both.
    let mut rows: Rows = BTreeMap::new();
    let mut scanned = 0usize;
    let mut horizontal = 0usize;

    for id in 0..reg.state_count() {
        let Some(state) = reg.resolve(id) else {
            continue;
        };
        let sm = models.state(state_id(id));
        if sm.quads.is_empty() {
            continue;
        }
        scanned += 1;
        for quad in &sm.quads {
            let Some(y) = horizontal_plane(&quad.positions) else {
                continue;
            };
            horizontal += 1;
            let d = distance_to_boundary(y);
            if d > NEAR_BOUNDARY {
                continue;
            }
            let key = (state.block.to_string(), format!("{d:.6}"), quad.cullface.is_some());
            let entry = rows.entry(key).or_insert((0, format!("y={y:.6}")));
            entry.0 += 1;
        }
    }
    (rows, scanned, horizontal)
}

#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn census_of_horizontal_quads_near_a_block_boundary() {
    let (models, reg) = build_models();
    let (rows, scanned, horizontal) = census(&models, reg.as_ref());

    // Liveness: a census that scanned nothing must fail, not print an empty
    // table and report a clean bill of health.
    assert!(
        scanned > 1000,
        "census scanned only {scanned} states with geometry — the bake did not run"
    );
    assert!(
        horizontal > 1000,
        "census found only {horizontal} horizontal quads — the planarity test is broken"
    );

    println!("\nstates with geometry: {scanned}, horizontal quads: {horizontal}\n");
    println!("{:<44} {:>10} {:>7} {:>7}  {}", "block", "dist", "culled", "quads", "plane");
    for ((block, dist, culled), (count, plane)) in &rows {
        println!("{block:<44} {dist:>10} {culled:>7} {count:>7}  {plane}");
    }

    println!("\n--- exactly on a boundary, no cullface (depth can never separate) ---");
    let mut exact = 0usize;
    for ((block, dist, culled), (count, _)) in &rows {
        if dist == "0.000000" && !culled {
            println!("  {block} ({count} quads)");
            exact += count;
        }
    }
    println!("  total quads: {exact}");
}

/// The blocks the report's family is made of, and the offset each one's
/// horizontal quads must sit at above the block floor. Every value is
/// vanilla's own model geometry (`template_leaf_litter_*` places its single
/// degenerate element at `from/to y = 0.25` in sixteenths, `carpet` at
/// `[0,0,0]..[16,1,16]`, `snow_height2` at `[0,0,0]..[16,2,16]`) — derived
/// from the jar's model JSON, not from anything this renderer produces.
const FAMILY: &[(&str, f32)] = &[
    ("minecraft:leaf_litter", 0.25 / 16.0),
    ("minecraft:white_carpet", 1.0 / 16.0),
    ("minecraft:moss_carpet", 1.0 / 16.0),
    ("minecraft:lily_pad", 0.25 / 16.0),
    ("minecraft:frogspawn", 0.25 / 16.0),
    ("minecraft:rail", 1.0 / 16.0),
];

/// The geometric half of the report's first hypothesis — *"our shape has been
/// flattened to zero height, so the plate is coplanar with the ground"* — as a
/// falsifiable claim: every family member's **uncullable** horizontal plane
/// must sit **strictly above** the block floor, at vanilla's own offset.
///
/// Quads carrying a `cullface` are excluded on purpose, and a carpet is why:
/// `carpet.json`'s bottom face *is* at `y = 0`, exactly coplanar with the
/// block below's top face, but it declares `cullface: down` and so is dropped
/// whenever that neighbour occludes — which is the only case where there is a
/// face to fight with. An uncullable quad has no such escape.
///
/// Expectation from the jar's model JSON ([`FAMILY`]), never from a sibling
/// value this renderer also produced.
#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn every_ground_plate_sits_at_vanillas_own_offset_above_the_block_floor() {
    let (models, reg) = build_models();
    let mut checked = 0usize;
    // Collect and assert on the collection: an `assert!` inside the loop would
    // prove exactly one arm and leave the rest of the family unmeasured.
    let mut failures = Vec::new();

    for &(block, want) in FAMILY {
        let mut seen = Vec::new();
        for id in 0..reg.state_count() {
            let Some(state) = reg.resolve(id) else { continue };
            if state.block.to_string() != block {
                continue;
            }
            for quad in &models.state(state_id(id)).quads {
                if quad.cullface.is_some() {
                    continue;
                }
                let Some(y) = horizontal_plane(&quad.positions) else {
                    continue;
                };
                let frac = y - y.floor();
                if frac > NEAR_BOUNDARY {
                    continue;
                }
                seen.push(frac);
            }
        }
        if seen.is_empty() {
            failures.push(format!("{block}: no near-floor horizontal quad baked at all"));
            continue;
        }
        checked += seen.len();
        let lowest = seen.iter().copied().fold(f32::INFINITY, f32::min);
        if lowest <= 0.0 {
            failures.push(format!(
                "{block}: a horizontal quad sits ON the block floor (y+{lowest}), coplanar with \
                 the block below's top face — the depth buffer can never separate them"
            ));
        } else if (lowest - want).abs() > 1e-6 {
            failures.push(format!(
                "{block}: lowest horizontal quad at y+{lowest}, vanilla's model says y+{want}"
            ));
        }
    }

    assert!(
        checked > 0,
        "no family quad was examined — the scan matched no block names"
    );
    assert!(failures.is_empty(), "ground-plate geometry: {failures:?}");
    println!("{checked} near-floor horizontal quads across {} blocks, all at vanilla's offset", FAMILY.len());
}
