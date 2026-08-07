//! Unit 9's central gates, re-pointed by the owner's ruling on #492: the ported
//! `Climate.RTree` must reproduce **vanilla's own indexed search**, and the
//! relationship to brute force is now a *measured divergence* rather than the
//! target.
//!
//! # What replaced what, and why the old gate was answering the wrong question
//!
//! The first landing asserted row-identity between the tree and
//! `nearest_row_brute_force` at every target, and that assertion held over 4.75M
//! targets. It was true, exhaustive, and aimed at the wrong reference: vanilla
//! ships both searches and calls the tree, and the two resolve to different biome
//! ids at 0.98% of arbitrary climate targets. So row-identity with brute force was
//! a guarantee of matching the search vanilla does *not* use.
//!
//! What survives unchanged is the part that was actually load-bearing — span
//! containment, and the exhaustive enumeration that verifies it. What it now buys
//! is stated more carefully:
//!
//! | claim | strength | gate |
//! |---|---|---|
//! | every node's span contains its children's | **theorem premise**, enumerated over every node/child pair × 7 axes | [`the_real_tree_is_shaped_right_and_every_node_contains_its_children`] |
//! | the tree finds the same *minimum squared distance* as brute force | **always**, at every target | [`the_minimum_distance_matches_brute_force_over_a_complete_lattice`] and the release sweeps |
//! | the tree returns the same *row* as brute force | **only where no tie exists** — this is the #492 divergence, measured not asserted | [`the_row_divergence_from_brute_force_is_exactly_the_tie_set`] |
//! | `lastResult` cannot change the distance | **always** | [`no_seed_can_change_the_minimum_distance`] |
//! | `lastResult` *can* change the row | demonstrated on a concrete case | [`a_tying_seed_changes_the_returned_row`] |
//!
//! # What "exhaustive" means here, and what it cannot mean
//!
//! The climate space is `20001^6 ≈ 6.4e28` targets, so **no gate can enumerate
//! it**, and one claiming to has miscounted. "We checked a lot of coordinates and
//! they agreed" is the *magnitude* species of vacuous test. So the distance claim
//! is a **theorem whose single premise is verified by complete enumeration**, and
//! the lattice sweeps corroborate it at three scales, each labelled with what it
//! actually covers: a complete 4⁶ lattice always-on, a complete 9⁶ lattice
//! (531,441 targets) and every integer step of `[-11000, 11000]` on each of the six
//! axes through 32 base points (4,224,192 targets) in release.
//!
//! # The table this runs against
//!
//! The real production asset,
//! `crates/lodestone-server/assets/worldgen/biome_parameters/overworld.json` — the
//! dump `scripts/worldgen-oracle/BiomeOracle.java` mode `table` produces and
//! `EmbeddedResolver::biome_parameters` serves. Read from the path because
//! `EmbeddedResolver` is private to `lodestone-server`.
//!
//! This is load-bearing against the **world** species of vacuous test: this crate's
//! own fixture resolvers supply *no* biome parameters, so a gate written against
//! them would build a one-row table, agree trivially, and prove nothing.
//! [`real_rows`] asserts the row count is 7,594 for that reason.

use std::path::PathBuf;

use lodestone_worldgen::biome::{BiomeParameterPoint, BiomeTable, nearest_row_brute_force};

/// Row count of the real overworld table — `BiomeOracle table`'s own measurement.
const REAL_TABLE_ROWS: usize = 7594;

fn real_table_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../lodestone-server/assets/worldgen/biome_parameters/overworld.json")
}

/// The real table's rows, in the asset's own order.
///
/// # Panics
/// Panics if the asset is missing or does not carry [`REAL_TABLE_ROWS`] rows — the
/// precondition check that keeps every gate below from passing vacuously.
fn real_rows() -> Vec<BiomeParameterPoint> {
    let path = real_table_path();
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("cannot read the real climate table at {}: {e}", path.display())
    });
    let json: serde_json::Value =
        serde_json::from_str(&raw).expect("the climate table must be JSON");
    let rows = lodestone_worldgen::biome::parse_table(&json);
    assert_eq!(
        rows.len(),
        REAL_TABLE_ROWS,
        "expected the real {REAL_TABLE_ROWS}-row overworld table from {}, got {} — a gate \
         against a short table proves nothing (the 'world' species of vacuous test)",
        path.display(),
        rows.len()
    );
    rows
}

fn real_table() -> BiomeTable {
    BiomeTable::new(real_rows())
}

/// A complete Cartesian lattice over the six climate axes: every combination of
/// `steps` evenly spaced values per axis, spanning `[-extent, extent]`. The seventh
/// slot (offset) is always `0` — a *target* never carries an offset.
fn lattice(steps: i64, extent: i64) -> Vec<[i64; 7]> {
    assert!(steps >= 2);
    let axis: Vec<i64> = (0..steps)
        .map(|i| -extent + (2 * extent * i) / (steps - 1))
        .collect();
    let mut out = Vec::with_capacity((steps as usize).pow(6));
    for &t in &axis {
        for &h in &axis {
            for &c in &axis {
                for &e in &axis {
                    for &d in &axis {
                        for &w in &axis {
                            out.push([t, h, c, e, d, w, 0]);
                        }
                    }
                }
            }
        }
    }
    out
}

/// A deterministic spread of arbitrary (non-round) targets. A fixed LCG, so the set
/// is identical on every machine and every run — and non-round matters: a regular
/// lattice inflates exact ties through symmetry, so these are the targets that
/// bound the real divergence.
fn arbitrary_targets(n: usize) -> Vec<[i64; 7]> {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((state >> 33) % 22001) as i64 - 11000
    };
    (0..n)
        .map(|_| [next(), next(), next(), next(), next(), next(), 0])
        .collect()
}

/// How many rows achieve the minimum fitness at `target`, and whether they all
/// carry the same biome id. A tie across *identical* biome ids is invisible.
fn tie_shape(rows: &[BiomeParameterPoint], target: &[i64; 7], best: i64) -> (usize, bool) {
    let tied: Vec<&BiomeParameterPoint> =
        rows.iter().filter(|r| r.fitness(target) == best).collect();
    let across_biomes = tied.iter().any(|r| r.biome != tied[0].biome);
    (tied.len(), across_biomes)
}

/// The theorem's premise, checked by complete enumeration, plus the tree's shape.
///
/// Every `(parent, child)` edge in the real tree, on all 7 axes: a subtree's span
/// must contain each child's, or `Node::distance` stops lower-bounding the leaves
/// beneath it and a prune could discard the true nearest biome. This is the only
/// assumption the distance claim rests on — and it is also what makes vanilla's
/// `lastResult` unable to change the distance — so it is checked exhaustively
/// rather than argued.
#[test]
fn the_real_tree_is_shaped_right_and_every_node_contains_its_children() {
    let table = real_table();
    let (nodes, leaves) = table.tree_shape();
    assert_eq!(
        leaves, REAL_TABLE_ROWS,
        "every table row must become exactly one leaf: no row dropped, none duplicated"
    );
    assert!(
        nodes > leaves,
        "a {REAL_TABLE_ROWS}-leaf tree must have interior nodes, got {nodes} nodes"
    );
    let violations = table.hull_containment_violations();
    assert!(
        violations.is_empty(),
        "hull containment is violated at {} (parent, child, axis) triples; first few: {:?}",
        violations.len(),
        &violations[..violations.len().min(8)]
    );
}

/// Control for the gate above, in both directions. Collapsing an **interior** node's
/// span must be reported; collapsing a **leaf** must not be, because a narrower
/// child is still inside its parent's hull — the more interesting half, since it
/// shows the detector reports the actual property rather than "something changed".
#[test]
fn hull_containment_control_fires_on_a_perturbed_interior_node() {
    let clean = real_table();
    assert!(clean.hull_containment_violations().is_empty());
    let count = clean.tree_node_count();
    assert!(!clean.tree_node_is_leaf(0), "node 0 must be the root");
    assert!(
        clean.tree_node_is_leaf(count - 1),
        "the last flattened node must be a leaf"
    );

    let mut perturbed_root = real_table();
    perturbed_root.perturb_tree_node(0);
    assert!(
        !perturbed_root.hull_containment_violations().is_empty(),
        "collapsing the root's span must be reported as a containment violation"
    );

    let mut perturbed_leaf = real_table();
    perturbed_leaf.perturb_tree_node(count - 1);
    assert!(
        perturbed_leaf.hull_containment_violations().is_empty(),
        "narrowing a leaf keeps it inside its parent, so the detector must stay silent — \
         firing here would mean it reports change rather than containment"
    );
}

/// **The distance claim, always-on**: over every point of a complete 4⁶ lattice, the
/// row vanilla's search selects sits at exactly the minimum squared distance brute
/// force finds. This is the assertion that survived #492 unchanged in strength.
#[test]
fn the_minimum_distance_matches_brute_force_over_a_complete_lattice() {
    let rows = real_rows();
    let table = BiomeTable::new(rows.clone());
    let targets = lattice(4, 11000);
    assert_eq!(targets.len(), 4096, "the lattice must be complete");
    let mut bad = 0usize;
    let mut first: Option<([i64; 7], i64, i64)> = None;
    for target in &targets {
        let (_, dist) = table.nearest_row_and_distance(target);
        let brute = nearest_row_brute_force(&rows, target);
        let expected = rows[brute as usize].fitness(target);
        if dist != expected {
            bad += 1;
            first.get_or_insert((*target, dist, expected));
        }
    }
    assert_eq!(
        bad, 0,
        "the tree found a different minimum distance at {bad} of {} targets; first: {first:?}",
        targets.len()
    );
}

/// The control that makes every distance assertion in this file evidence rather
/// than description. Collapsing one interior node's span breaks the lower-bound
/// property, so pruning becomes unsound and the tree starts reporting distances
/// brute force does not.
///
/// **Which node to perturb is the whole design of this control**, and *three*
/// natural choices are premise-false in the safe-looking direction. All three were
/// measured at 0 wrong distances before this landed on one that fires:
///
/// | perturbed | wrong distances | why it cannot fire |
/// |---|---|---|
/// | a **leaf** (last flattened node) | 0 | narrowing a leaf only makes it less attractive, and 1 row of 7,594 never wins on this lattice |
/// | the **root** (node 0) | 0 | `search` iterates the root's children and never prunes on the root's own bound |
/// | the root's **first** child (node 1) | 0 | `best_dist` starts at `i64::MAX`, so `best_dist > child_bound` is unconditionally true for the first child — **it can never be pruned** |
///
/// A broken bound is only observable through a **prune**, so the control has to
/// collapse a node that can actually be pruned: a *later* child of the root, which
/// is only examined once `best_dist` is finite. That is the third row's lesson and
/// the reason this test walks `tree_root_child_nodes()` from the back.
#[test]
fn distance_control_fires_when_a_prunable_tree_node_is_perturbed() {
    let rows = real_rows();
    let targets = lattice(4, 11000);
    let children = BiomeTable::new(rows.clone()).tree_root_child_nodes();
    assert!(
        children.len() >= 2,
        "the root must have several children for a prunable one to exist, got {}",
        children.len()
    );

    // Later children first: those are the ones a finite `best_dist` can prune.
    let mut fired: Option<(u32, usize)> = None;
    for &child in children.iter().rev() {
        let mut table = BiomeTable::new(rows.clone());
        table.perturb_tree_node(child as usize);
        let mut bad = 0usize;
        for target in &targets {
            let (_, dist) = table.nearest_row_and_distance(target);
            let brute = nearest_row_brute_force(&rows, target);
            if dist != rows[brute as usize].fitness(target) {
                bad += 1;
            }
        }
        if bad > 0 {
            fired = Some((child, bad));
            break;
        }
    }
    let (child, bad) = fired.expect(
        "collapsing every child of the root produced 0 wrong distances over the lattice, so \
         the distance gates in this file prove nothing — the control's premise is false",
    );
    eprintln!(
        "[U9 control] collapsing root child node {child} produced {bad} wrong distances over \
         {} targets",
        targets.len()
    );
}

/// **The #492 divergence, characterised rather than tolerated.** Where the tree and
/// brute force pick different rows, it must always be because several rows tie on
/// the minimum distance — never because one of them found a nearer row. That
/// distinction is the whole reason this change is safe to make: the tree is not
/// finding a *different nearest*, it is taking a different member of the tied set,
/// which is exactly what vanilla does.
#[test]
fn the_row_divergence_from_brute_force_is_exactly_the_tie_set() {
    let rows = real_rows();
    let table = BiomeTable::new(rows.clone());
    let targets = lattice(4, 11000);
    let mut row_differs = 0usize;
    let mut biome_differs = 0usize;
    let mut differs_without_a_tie = 0usize;
    for target in &targets {
        let (tree_row, dist) = table.nearest_row_and_distance(target);
        let brute = nearest_row_brute_force(&rows, target);
        if tree_row == brute {
            continue;
        }
        row_differs += 1;
        if table.biome_at(tree_row) != table.biome_at(brute) {
            biome_differs += 1;
        }
        // The load-bearing assertion: a disagreement at a *unique* minimum would
        // mean one of the two searches is simply wrong.
        let (tied, _) = tie_shape(&rows, target, dist);
        if tied < 2 {
            differs_without_a_tie += 1;
        }
    }
    assert_eq!(
        differs_without_a_tie, 0,
        "the tree and brute force disagreed at {differs_without_a_tie} targets with a UNIQUE \
         minimum — that is not a tie-break difference, it means one search is wrong"
    );
    eprintln!(
        "[U9 #492] 4^6 lattice: {row_differs} row disagreements, {biome_differs} of them \
         resolving to a different biome id, 0 at a unique minimum"
    );
}

/// The first `lastResult` claim: a seed can never change the minimum distance,
/// because a node bound lower-bounds its leaves, so a subtree skipped by a
/// higher incumbent held nothing better. Checked with adversarial seeds.
#[test]
fn no_seed_can_change_the_minimum_distance() {
    let rows = real_rows();
    let table = BiomeTable::new(rows.clone());
    let nodes = table.tree_node_count() as u32;
    let mut checked = 0usize;
    for (i, target) in arbitrary_targets(96).iter().enumerate() {
        let brute = nearest_row_brute_force(&rows, target);
        let expected = rows[brute as usize].fitness(target);
        for seed in [None, Some(0), Some(1), Some(nodes - 1), Some((i as u32 * 977) % nodes)] {
            let row = table.nearest_row_seeded(target, seed);
            assert_eq!(
                rows[row as usize].fitness(target),
                expected,
                "seed {seed:?} changed the minimum distance at {target:?}"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 96 * 5, "the sweep must actually run");
}

/// The second `lastResult` claim, and the reason vanilla's search is **not** a pure
/// function of its target: seeding the incumbent with a leaf that *ties* the minimum
/// makes vanilla return that leaf instead of the one tree order would have chosen.
///
/// This is the finding that decides which vanilla behaviour we can implement, so it
/// is exhibited on a concrete case rather than argued. If this test ever fails to
/// find a case, the premise of the module doc's "deliberately not implemented"
/// paragraph is false and the seeding could be reproduced after all — so a failure
/// here is informative, not merely red.
#[test]
fn a_tying_seed_changes_the_returned_row() {
    let rows = real_rows();
    let table = BiomeTable::new(rows.clone());
    let mut demonstrated: Option<([i64; 7], u32, u32)> = None;
    for target in arbitrary_targets(20_000) {
        let (unseeded, dist) = table.nearest_row_and_distance(&target);
        let (tied, _) = tie_shape(&rows, &target, dist);
        if tied < 2 {
            continue;
        }
        // Find a *different* row at the same minimum distance and seed with it.
        let other = rows
            .iter()
            .position(|r| r.fitness(&target) == dist)
            .map(|i| i as u32)
            .filter(|&i| i != unseeded);
        let Some(other) = other else { continue };
        let Some(seed_node) = table.leaf_node_for_row(other) else {
            continue;
        };
        let seeded = table.nearest_row_seeded(&target, Some(seed_node));
        if seeded != unseeded {
            demonstrated = Some((target, unseeded, seeded));
            break;
        }
    }
    let (target, unseeded, seeded) = demonstrated.expect(
        "no target found where a tying seed changes the returned row — if this is really \
         impossible then vanilla's lastResult is a pure optimisation and could be ported",
    );
    assert_ne!(unseeded, seeded);
    eprintln!(
        "[U9 lastResult] at {target:?}: unseeded returns row {unseeded}, a tying seed returns \
         row {seeded} — vanilla's search is history-dependent, so this port implements the \
         fresh-instance (unseeded) answer only"
    );
}

/// Exhaustive over a complete 9-per-axis lattice: 9⁶ = 531,441 targets, every
/// combination. Release-only because each target costs a 7,594-row brute-force scan.
#[test]
#[ignore = "531,441 targets x a 7,594-row brute-force scan each; release profile"]
fn the_minimum_distance_matches_brute_force_over_a_complete_nine_per_axis_lattice() {
    let rows = real_rows();
    let table = BiomeTable::new(rows.clone());
    let targets = lattice(9, 11000);
    assert_eq!(targets.len(), 531_441);
    let mut wrong_distance = 0usize;
    let mut differs_without_a_tie = 0usize;
    let mut row_differs = 0usize;
    let mut biome_differs = 0usize;
    let mut tied_across_biomes = 0usize;
    for target in &targets {
        let (tree_row, dist) = table.nearest_row_and_distance(target);
        let brute = nearest_row_brute_force(&rows, target);
        let expected = rows[brute as usize].fitness(target);
        if dist != expected {
            wrong_distance += 1;
        }
        let (tied, across) = tie_shape(&rows, target, expected);
        if across {
            tied_across_biomes += 1;
        }
        if tree_row != brute {
            row_differs += 1;
            if table.biome_at(tree_row) != table.biome_at(brute) {
                biome_differs += 1;
            }
            if tied < 2 {
                differs_without_a_tie += 1;
            }
        }
    }
    println!(
        "9^6 lattice ({} targets):\n  wrong minimum distance:      {}\n  \
         row disagreements vs brute:  {}\n  ...resolving to a different biome id: {}\n  \
         disagreements at a UNIQUE minimum: {}\n  targets tied across biome ids: {}",
        targets.len(),
        wrong_distance,
        row_differs,
        biome_differs,
        differs_without_a_tie,
        tied_across_biomes
    );
    assert_eq!(wrong_distance, 0, "the tree must always find the minimum distance");
    assert_eq!(
        differs_without_a_tie, 0,
        "every row disagreement must be a tie-break, never a different nearest"
    );
}

/// Exhaustive **along** every axis: for each of the six axes and each of 32 base
/// points, every integer coordinate in `[-11000, 11000]` at unit resolution —
/// 4,224,192 targets. This is the sweep that would catch a pruning flaw showing only
/// at a specific coordinate.
#[test]
#[ignore = "4,224,192 targets x a 7,594-row brute-force scan each; release profile"]
fn the_minimum_distance_matches_brute_force_at_every_unit_step_along_every_axis() {
    let rows = real_rows();
    let table = BiomeTable::new(rows.clone());
    let bases = arbitrary_targets(32);
    let mut total = 0usize;
    let mut wrong_distance = 0usize;
    let mut differs_without_a_tie = 0usize;
    for axis in 0..6usize {
        for base in &bases {
            for v in -11000..=11000i64 {
                let mut target = *base;
                target[axis] = v;
                let (tree_row, dist) = table.nearest_row_and_distance(&target);
                let brute = nearest_row_brute_force(&rows, &target);
                let expected = rows[brute as usize].fitness(&target);
                if dist != expected {
                    wrong_distance += 1;
                }
                if tree_row != brute && tie_shape(&rows, &target, expected).0 < 2 {
                    differs_without_a_tie += 1;
                }
                total += 1;
            }
        }
    }
    assert_eq!(total, 6 * 32 * 22_001);
    println!(
        "per-axis unit sweeps: {total} targets, {wrong_distance} wrong distances, \
         {differs_without_a_tie} disagreements at a unique minimum"
    );
    assert_eq!(wrong_distance, 0);
    assert_eq!(differs_without_a_tie, 0);
}

/// The divergence figure the docs and #492 quote, measured on arbitrary (non-round)
/// targets — the ones that bound the production risk, since a regular lattice
/// inflates exact ties through symmetry.
#[test]
#[ignore = "200,000 targets x a full tie census each; release profile"]
fn the_divergence_from_brute_force_on_arbitrary_targets() {
    let rows = real_rows();
    let table = BiomeTable::new(rows.clone());
    let targets = arbitrary_targets(200_000);
    let mut row_differs = 0usize;
    let mut biome_differs = 0usize;
    let mut differs_without_a_tie = 0usize;
    let mut tied_across_biomes = 0usize;
    for target in &targets {
        let (tree_row, dist) = table.nearest_row_and_distance(target);
        let brute = nearest_row_brute_force(&rows, target);
        assert_eq!(
            dist,
            rows[brute as usize].fitness(target),
            "distance mismatch at {target:?}"
        );
        let (tied, across) = tie_shape(&rows, target, dist);
        if across {
            tied_across_biomes += 1;
        }
        if tree_row != brute {
            row_differs += 1;
            if table.biome_at(tree_row) != table.biome_at(brute) {
                biome_differs += 1;
            }
            if tied < 2 {
                differs_without_a_tie += 1;
            }
        }
    }
    println!(
        "{} arbitrary targets: {} row disagreements, {} resolving to a different biome id \
         ({:.2}%), {} at a unique minimum, {} tied across biome ids",
        targets.len(),
        row_differs,
        biome_differs,
        100.0 * biome_differs as f64 / targets.len() as f64,
        differs_without_a_tie,
        tied_across_biomes
    );
    assert_eq!(differs_without_a_tie, 0);
}

/// **The production-impact measurement for #492.** How often does the tie-break
/// change actually change a biome at coordinates the world really generates?
///
/// The 0.98% figure is over *arbitrary* climate targets. Real climate is
/// `(f32 * 10000) as i64` evaluated at real positions, and whether it ever lands on
/// an exact tie between two rows carrying different biome ids is a separate,
/// unanswered question — and the one that decides whether any fixture needs
/// re-baselining. "No gate failed" is not an answer to it: the gates cover a few
/// specific regions, so silence there is weak evidence.
///
/// Both consumer conventions are measured, because they sample at different heights
/// and the plan is explicit that they must not be unified: the **carver/ore** source
/// biome at `y = 0`, and the **surface** biome at each quart's own generated height.
#[test]
#[ignore = "generates real embedded terrain over a region; release profile"]
fn the_tiebreak_moves_exactly_the_eight_recorded_source_biomes_and_no_surface_quart() {
    let generator = lodestone_server::overworld_generator(42);

    // Carver/ore convention: y = 0, no terrain needed, so this can cover a wide area.
    let mut source_checked = 0usize;
    let mut differing_sources: Vec<(i32, i32, String, String)> = Vec::new();
    for cx in -64..64i32 {
        for cz in -64..64i32 {
            match generator.source_biome_tiebreak(cx, cz) {
                None => panic!(
                    "the embedded generator must have a real climate table; a fixed-biome \
                     generator makes this gate vacuous (the 'world' species)"
                ),
                Some((tree, brute)) => {
                    source_checked += 1;
                    if tree != brute {
                        differing_sources.push((cx, cz, tree.to_string(), brute.to_string()));
                    }
                }
            }
        }
    }
    assert_eq!(source_checked, 128 * 128, "the source sweep must actually run");

    // Surface convention: needs generated heights, so a smaller region.
    let mut surface_quarts = 0usize;
    let mut surface_differing = 0usize;
    for cx in -6..6i32 {
        for cz in -6..6i32 {
            let (quarts, differing) = generator.surface_biome_tiebreak_differences(cx, cz);
            surface_quarts += quarts;
            surface_differing += differing;
        }
    }
    assert_eq!(surface_quarts, 12 * 12 * 16, "the surface sweep must actually run");

    println!(
        "#492 production impact: carver/ore sources {}/{source_checked} differ at {:?}; \
         surface quarts {surface_differing}/{surface_quarts} differ",
        differing_sources.len(),
        differing_sources
    );
    for (cx, cz, tree, brute) in &differing_sources {
        println!(
            "  source ({cx}, {cz}) -> JVM sample coords ({}, 0, {}): tree={tree} brute={brute}",
            cx * 16,
            cz * 16
        );
    }

    // **Recorded measurements of an intended divergence, not targets.** The owner
    // ruled we match vanilla's tree, so these coordinates *should* move; what must
    // not happen silently is the set changing size, which would mean the tie-break
    // or the tree's child order moved again. Both numbers are deterministic (seed
    // 42, fixed regions), so they are asserted exactly rather than bounded.
    //
    // The asymmetry is the useful part and it is why both conventions are measured:
    // the carver/ore biome is sampled at `y = 0` where `depth` is already ~+1.0, deep
    // in cave climate-space where the table's rows crowd together and exact ties are
    // reachable; the surface convention samples near each column's own terrain, where
    // over this region no tie occurred at all. So this change moves carver and ore
    // selection at a handful of chunks and moves the biome a player sees nowhere in
    // the measured region.
    assert_eq!(
        differing_sources.len(),
        8,
        "expected the 8 recorded divergent carver/ore source chunks in this region, got {:?} \
         — if the tie-break or child order changed, re-verify against the JVM oracle and \
         update this number deliberately",
        differing_sources
    );
    assert_eq!(
        surface_differing, 0,
        "no surface quart diverged in this region when measured; a non-zero count here means \
         the player-visible biome moved and every affected fixture needs re-baselining"
    );
}

/// **The JVM fixture for #492, taken at the coordinates that discriminate.**
///
/// A fixture at arbitrary coordinates would prove almost nothing here: vanilla's two
/// searches agree at over 99% of real source chunks, so a randomly chosen probe set
/// passes under *either* tie-break. These eight are the complete set of carver/ore
/// source chunks in a 128×128 region where they disagree — found by
/// [`the_tiebreak_moves_exactly_the_eight_recorded_source_biomes_and_no_surface_quart`], which prints
/// the coordinates for exactly this purpose.
///
/// Ground truth: `scripts/worldgen-oracle/BiomeOracle.java sample 42 <x> 0 <z>`, run
/// **once per coordinate in its own JVM process**. That is not incidental — within a
/// single process vanilla's `findValue` for sample *N* is seeded by sample *N−1*'s
/// leaf via `RTree.lastResult`, so only the first sample of a process is the
/// fresh-instance answer this port implements. A batched oracle run would have
/// produced history-contaminated expectations.
///
/// Each row asserts **both** directions, which is what makes it a characterisation of
/// the divergence rather than a one-sided check:
///
/// * `indexed` — vanilla's real answer, and what this engine must now produce;
/// * `brute` — vanilla's reference answer, and what this engine produced before #492.
///
/// If a future change made the two coincide, this test fails rather than silently
/// passing, because it asserts the brute-force column too.
#[test]
fn vanilla_tree_fixture_at_the_eight_divergent_source_chunks() {
    // (source cx, source cz, vanilla findValue, vanilla findValueBruteForce)
    let cases: &[(i32, i32, &str, &str)] = &[
        (26, -41, "minecraft:sunflower_plains", "minecraft:plains"),
        (31, 58, "minecraft:river", "minecraft:ocean"),
        (39, -60, "minecraft:forest", "minecraft:meadow"),
        (43, 61, "minecraft:river", "minecraft:beach"),
        (44, -2, "minecraft:deep_ocean", "minecraft:deep_cold_ocean"),
        (51, -18, "minecraft:cold_ocean", "minecraft:deep_cold_ocean"),
        (52, 15, "minecraft:ocean", "minecraft:cold_ocean"),
        (57, 25, "minecraft:ocean", "minecraft:cold_ocean"),
    ];
    let generator = lodestone_server::overworld_generator(42);
    for &(cx, cz, want_indexed, want_brute) in cases {
        let (got_indexed, got_brute) = generator
            .source_biome_tiebreak(cx, cz)
            .expect("the embedded generator must carry a real climate table");
        assert_eq!(
            got_indexed, want_indexed,
            "source chunk ({cx}, {cz}) must resolve to vanilla's own findValue answer \
             (JVM: {want_indexed}); got {got_indexed}"
        );
        assert_eq!(
            got_brute, want_brute,
            "source chunk ({cx}, {cz}): the brute-force reference must still reproduce \
             vanilla's findValueBruteForce (JVM: {want_brute}); got {got_brute} — if this \
             moved, the divergence this fixture characterises has changed shape"
        );
        assert_ne!(
            want_indexed, want_brute,
            "({cx}, {cz}) is in this fixture because the two vanilla searches disagree there; \
             a row where they agree cannot discriminate between the two tie-breaks"
        );
    }
}
