//! Unit 9's central gate: the ported `Climate.RTree` returns **exactly** what
//! `crate::biome::nearest_row_brute_force` returns, on the real 7,594-row
//! overworld climate table.
//!
//! # What "exhaustive" means here, stated concretely
//!
//! The climate parameter space is six quantized axes over roughly
//! `[-10000, 10000]`. Enumerating it is `20001^6 ≈ 6.4e28` targets, so **no test
//! can enumerate the space**, and any gate that claims to has miscounted. Saying
//! "we sampled a lot of coordinates and they agreed" is the *magnitude* species of
//! vacuous test (CLAUDE.md): it proves the two searches usually agree.
//!
//! So identity here is a **theorem with an exhaustively-verified premise**, and
//! the target sweeps are corroboration, each labelled with what it really covers:
//!
//! | gate | what it covers | exhaustive over |
//! |---|---|---|
//! | [`the_real_tree_is_shaped_right_and_every_node_contains_its_children`] | the theorem's one premise: `space(child) ⊆ space(parent)` | **every** node/child pair × **every** axis in the real tree — a complete enumeration, 7,594 leaves and every interior node |
//! | [`hull_containment_control_fires_on_a_perturbed_node`] | that the premise check can fail | the control |
//! | [`identity_holds_over_a_complete_four_per_axis_lattice`] | tree == brute force | **every point** of a 4⁶ Cartesian lattice (4,096 targets), always-on |
//! | [`identity_control_fires_when_one_tree_node_is_perturbed`] | that the identity assertion can fail | the control, same lattice |
//! | [`identity_holds_over_a_complete_nine_per_axis_lattice`] | tree == brute force | **every point** of a 9⁶ lattice (531,441 targets), release, `#[ignore]`d |
//! | [`identity_holds_at_every_unit_step_along_every_axis`] | tree == brute force | **every integer coordinate** of `[-11000, 11000]` on each of the 6 axes, through 32 base points — 4.2M targets, release, `#[ignore]`d |
//! | [`vanillas_own_tree_and_brute_force_are_compared_on_the_real_table`] | whether *vanilla's* two searches agree | the same 9⁶ lattice |
//!
//! The theorem is what makes the claim total; the lattices are what would catch a
//! flaw in the theorem's reasoning. See `src/biome/tree.rs`'s module doc for the
//! proof and for why the search deliberately does **not** copy vanilla's
//! tie-break.
//!
//! # The table this runs against
//!
//! The real production asset,
//! `crates/lodestone-server/assets/worldgen/biome_parameters/overworld.json` —
//! the dump `scripts/worldgen-oracle/BiomeOracle.java` mode `table` produces and
//! `EmbeddedResolver::biome_parameters` serves. Read from the path rather than
//! through `lodestone-server` because `EmbeddedResolver` is private there.
//!
//! This is load-bearing against the **world** species of vacuous test: this
//! crate's own fixture resolvers supply *no* biome parameters at all
//! (`tests/support/worldgen_data` is single-biome plains), so a gate written
//! against them would build a one-row table, agree trivially, and prove nothing.
//! [`real_table`] asserts the row count is 7,594 for that reason — a wrong path or
//! an empty asset fails loudly instead of passing vacuously.

use std::path::PathBuf;

use lodestone_worldgen::biome::{BiomeParameterPoint, BiomeTable, nearest_row_brute_force};

/// Row count of the real overworld table — `BiomeOracle table`'s own measurement
/// (`src/biome/mod.rs`'s module doc records the 7,594 figure and that an earlier
/// "~700" estimate was off by 10×).
const REAL_TABLE_ROWS: usize = 7594;

fn real_table_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../lodestone-server/assets/worldgen/biome_parameters/overworld.json")
}

/// The real table's rows, in the asset's own order.
///
/// # Panics
/// Panics if the asset is missing or does not carry [`REAL_TABLE_ROWS`] rows —
/// the precondition check that keeps every gate below from passing vacuously.
fn real_rows() -> Vec<BiomeParameterPoint> {
    let path = real_table_path();
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read the real climate table at {}: {e}", path.display()));
    let json: serde_json::Value = serde_json::from_str(&raw).expect("the climate table must be JSON");
    let rows = lodestone_worldgen::biome::parse_table(&json);
    assert_eq!(
        rows.len(),
        REAL_TABLE_ROWS,
        "expected the real {REAL_TABLE_ROWS}-row overworld table from {}, got {} — a gate against \
         a short table proves nothing (the 'world' species of vacuous test)",
        path.display(),
        rows.len()
    );
    rows
}

fn real_table() -> BiomeTable {
    BiomeTable::new(real_rows())
}

/// A complete Cartesian lattice over the six climate axes: every combination of
/// `steps` evenly spaced values per axis, spanning `[-lo, lo]`. The seventh slot
/// (offset) is always `0` — a *target* never carries an offset, only a biome's own
/// parameter point does (`Climate.Sampler.sample`).
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

/// A deterministic spread of base points for the per-axis unit sweeps. Not random
/// in the sense of needing a seed recorded: a fixed LCG so the set is identical on
/// every machine and every run.
fn base_points(n: usize) -> Vec<[i64; 7]> {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((state >> 33) % 22001) as i64 - 11000
    };
    (0..n)
        .map(|_| [next(), next(), next(), next(), next(), next(), 0])
        .collect()
}

/// Compares tree against brute force over `targets`, returning the mismatches as
/// `(target, tree_row, brute_row)`.
///
/// Collects rather than asserting per target, so a failure can report **how many**
/// and **where** — a count plus concrete coordinates, not "assertion failed".
fn mismatches(
    table: &BiomeTable,
    rows: &[BiomeParameterPoint],
    targets: &[[i64; 7]],
) -> Vec<([i64; 7], u32, u32)> {
    let mut out = Vec::new();
    for target in targets {
        let tree = table.nearest_row(target);
        let brute = nearest_row_brute_force(rows, target);
        if tree != brute {
            out.push((*target, tree, brute));
            if out.len() >= 32 {
                break;
            }
        }
    }
    out
}

fn report(label: &str, bad: &[([i64; 7], u32, u32)], total: usize) -> String {
    let mut s = format!("{label}: {} of {total} targets disagreed", bad.len());
    for (target, tree, brute) in bad.iter().take(8) {
        s.push_str(&format!("\n  at {target:?}: tree row {tree}, brute row {brute}"));
    }
    s
}

/// The theorem's premise, checked by complete enumeration, plus the tree's shape.
///
/// Every `(parent, child)` edge in the real tree, on all 7 axes: a subtree's span
/// must contain each child's, or `Node::distance` stops lower-bounding the leaves
/// beneath it and a prune could discard the true nearest biome. This is the only
/// assumption `nearest_row`'s identity to brute force rests on
/// (`src/biome/tree.rs`'s module doc), and it is checkable exhaustively — so it
/// is checked exhaustively rather than argued.
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
        "a {REAL_TABLE_ROWS}-leaf tree must have interior nodes, got {nodes} nodes for {leaves} leaves"
    );
    let violations = table.hull_containment_violations();
    assert!(
        violations.is_empty(),
        "hull containment is violated at {} (parent, child, axis) triples; first few: {:?}",
        violations.len(),
        &violations[..violations.len().min(8)]
    );
}

/// Control for the gate above, in both directions.
///
/// Collapsing an **interior** node's span to a point makes it stop containing its
/// children, and must be reported. Collapsing a **leaf** must *not* be: a narrower
/// child is still inside its parent's hull, so containment genuinely still holds —
/// which is the more interesting half, because it shows the detector is reporting
/// the actual property rather than "something changed".
///
/// Nodes are flattened in DFS pre-order, so node 0 is the root and the last node
/// is a leaf; both facts are asserted rather than assumed.
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
    let root_violations = perturbed_root.hull_containment_violations();
    assert!(
        !root_violations.is_empty(),
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

/// Always-on identity: **every point** of a complete 4-per-axis lattice
/// (4⁶ = 4,096 targets) spanning the full quantized range.
#[test]
fn identity_holds_over_a_complete_four_per_axis_lattice() {
    let rows = real_rows();
    let table = BiomeTable::new(rows.clone());
    let targets = lattice(4, 11000);
    assert_eq!(targets.len(), 4096, "the lattice must be complete");
    let bad = mismatches(&table, &rows, &targets);
    assert!(
        bad.is_empty(),
        "{}",
        report("4^6 lattice", &bad, targets.len())
    );
}

/// The control for the gate above, and the one that makes every identity claim in
/// this file evidence rather than description: perturbing a single tree node
/// breaks the lower-bound property, so pruning becomes unsound and the tree starts
/// answering differently from brute force. Observed to fire — the assertion is on
/// the mismatch count being **non-zero**.
#[test]
fn identity_control_fires_when_one_tree_node_is_perturbed() {
    let rows = real_rows();
    let targets = lattice(4, 11000);
    let clean = {
        let table = BiomeTable::new(rows.clone());
        mismatches(&table, &rows, &targets).len()
    };
    assert_eq!(clean, 0, "precondition: the unperturbed tree must agree");

    // **Which node to perturb is the whole design of this control**, and the first
    // two choices were premise-false in the safe-looking direction:
    //
    // * A *leaf* (the last flattened node) only narrows its own span, so it
    //   becomes less attractive and, being 1 row of 7,594, never wins on this
    //   lattice — measured 0 mismatches. Reads as "identity is robust".
    // * The *root* (node 0) is descended into unconditionally — `nearest_row`
    //   never prunes on the root's own bound — so collapsing it changes nothing.
    //   Also measured 0 mismatches.
    //
    // The node whose bound actually gates a prune is an interior node **below**
    // the root, so that is what this collapses: node 1, the root's first child in
    // DFS pre-order, covering roughly a sixth of the table.
    let mut table = BiomeTable::new(rows.clone());
    assert!(
        !table.tree_node_is_leaf(1),
        "node 1 must be an interior subtree for this control to test pruning"
    );
    table.perturb_tree_node(1);
    let bad = mismatches(&table, &rows, &targets);
    assert!(
        !bad.is_empty(),
        "the identity comparison must be able to fail: collapsing interior node 1 produced 0 \
         mismatches over {} targets, so the gates above prove nothing",
        targets.len()
    );
    eprintln!(
        "[U9 control] collapsing interior node 1 produced {}+ mismatches over {} targets \
         (collection stops at 32)",
        bad.len(),
        targets.len()
    );
}

/// The answer is a lexicographic `(distance, row)` min-reduction, so it cannot
/// depend on the seed hint — which is what licenses sharing the hint across
/// threads in a `Relaxed` atomic instead of a `ThreadLocal`, and what stops
/// vanilla's history-dependent `lastResult` behaviour from entering this engine.
/// Checked against brute force under adversarial seeds, including the last leaf
/// and a rotating one.
#[test]
fn the_result_is_invariant_under_every_search_seed() {
    let rows = real_rows();
    let table = BiomeTable::new(rows.clone());
    let nodes = table.tree_node_count() as u32;
    let mut checked = 0usize;
    for (i, target) in base_points(96).iter().enumerate() {
        let expected = nearest_row_brute_force(&rows, target);
        for hint in [u32::MAX, 0, 1, nodes - 1, (i as u32 * 977) % nodes] {
            table.force_seed_hint(hint);
            assert_eq!(
                table.nearest_row(target),
                expected,
                "seed hint {hint} changed the answer at {target:?}"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 96 * 5, "the sweep must actually run");
}

/// Exhaustive over a complete 9-per-axis lattice: 9⁶ = 531,441 targets, every
/// combination. Release-only because each target costs a 7,594-row brute-force
/// scan (~4e10 operations in total).
#[test]
#[ignore = "531,441 targets x a 7,594-row brute-force scan each; release profile"]
fn identity_holds_over_a_complete_nine_per_axis_lattice() {
    let rows = real_rows();
    let table = BiomeTable::new(rows.clone());
    let targets = lattice(9, 11000);
    assert_eq!(targets.len(), 531_441);
    let bad = mismatches(&table, &rows, &targets);
    assert!(bad.is_empty(), "{}", report("9^6 lattice", &bad, targets.len()));
    println!("9^6 lattice: {} targets, 0 disagreements", targets.len());
}

/// Exhaustive **along** every axis: for each of the six axes and each of 32 base
/// points, every integer coordinate in `[-11000, 11000]` — the full quantized
/// range plus a 1,000-unit margin past it on both sides, at unit resolution. 6 × 32
/// × 22,001 = 4,224,192 targets.
///
/// This is the sweep that would catch a tie-break or pruning flaw that only shows
/// at a specific coordinate: every breakpoint of the piecewise-linear per-axis
/// distance function, and every integer between them, is visited.
#[test]
#[ignore = "4,224,192 targets x a 7,594-row brute-force scan each; release profile"]
fn identity_holds_at_every_unit_step_along_every_axis() {
    let rows = real_rows();
    let table = BiomeTable::new(rows.clone());
    let bases = base_points(32);
    let mut total = 0usize;
    for axis in 0..6usize {
        for base in &bases {
            let targets: Vec<[i64; 7]> = (-11000..=11000i64)
                .map(|v| {
                    let mut t = *base;
                    t[axis] = v;
                    t
                })
                .collect();
            let bad = mismatches(&table, &rows, &targets);
            assert!(
                bad.is_empty(),
                "{}",
                report(&format!("axis {axis} unit sweep from {base:?}"), &bad, targets.len())
            );
            total += targets.len();
        }
    }
    assert_eq!(total, 6 * 32 * 22_001);
    println!("per-axis unit sweeps: {total} targets, 0 disagreements");
}

/// The question `src/biome/tree.rs`'s module doc raises, measured rather than
/// assumed: **does vanilla's own `RTree` ever disagree with vanilla's own
/// `findValueBruteForce`?** Vanilla's search prunes strictly and lets the incumbent
/// win a tie, so the two can differ in principle wherever two rows tie on squared
/// distance.
///
/// This does not assert agreement — if vanilla's two searches differ somewhere,
/// that is a finding about vanilla, not a defect here, and the JVM oracle
/// (`BiomeOracle sample`, which dumps *both*) is the tie-breaker per the plan's
/// Q4.5. What it does assert is that our own search agrees with brute force on the
/// same space, and it **reports** vanilla's number so the tie-break decision rests
/// on a measurement.
#[test]
#[ignore = "531,441 targets x two searches plus a brute-force scan each; release profile"]
fn vanillas_own_tree_and_brute_force_are_compared_on_the_real_table() {
    let rows = real_rows();
    let table = BiomeTable::new(rows.clone());
    let targets = lattice(9, 11000);
    let mut vanilla_differs_by_row = 0usize;
    let mut vanilla_differs_by_biome = 0usize;
    let mut ours_differs = 0usize;
    let mut tied_targets = 0usize;
    let mut tied_across_biomes = 0usize;
    let mut examples: Vec<([i64; 7], String, String)> = Vec::new();
    for target in &targets {
        let brute = nearest_row_brute_force(&rows, target);
        if table.nearest_row(target) != brute {
            ours_differs += 1;
        }
        let vanilla = table.nearest_row_vanilla_exact(target, None);
        if vanilla != brute {
            vanilla_differs_by_row += 1;
            // **The only question that can reach a block.** Two table rows can
            // carry the same biome id with different climate spans (the asset's
            // first two rows are both `mushroom_fields`), so a row-level
            // disagreement is invisible unless the *names* differ too.
            let a = table.biome_at(vanilla);
            let b = table.biome_at(brute);
            if a != b {
                vanilla_differs_by_biome += 1;
                if examples.len() < 8 {
                    examples.push((*target, a.to_string(), b.to_string()));
                }
            }
        }
        // A tie census: how many targets have more than one row at the minimum
        // squared distance, and of those, how many tie across *different* biomes?
        // The second number is what makes a tie-break choice observable at all.
        let best = rows[brute as usize].fitness(target);
        let tied: Vec<&BiomeParameterPoint> =
            rows.iter().filter(|r| r.fitness(target) == best).collect();
        if tied.len() > 1 {
            tied_targets += 1;
            if tied.iter().any(|r| r.biome != tied[0].biome) {
                tied_across_biomes += 1;
            }
        }
    }
    println!(
        "9^6 lattice ({} targets):\n  ours-vs-brute:            {} disagreements\n  \
         vanilla's-tree-vs-brute:  {} by row, {} by BIOME ID\n  tied minimum:             {} \
         targets, of which {} tie across different biome ids\n  biome-level examples: {:?}",
        targets.len(),
        ours_differs,
        vanilla_differs_by_row,
        vanilla_differs_by_biome,
        tied_targets,
        tied_across_biomes,
        examples
    );
    assert_eq!(
        ours_differs, 0,
        "our search must agree with brute force everywhere on this lattice"
    );
    // A self-check on the vanilla-exact port, which is the only unvalidated code
    // in this test: at a target with a *unique* minimum, both searches must find
    // it, so vanilla can only differ where a tie exists. If this inequality ever
    // broke, the 16,526 figure would be a bug in the diagnostic rather than a fact
    // about vanilla.
    assert!(
        vanilla_differs_by_biome <= tied_across_biomes,
        "vanilla's search differed at {vanilla_differs_by_biome} targets but only \
         {tied_across_biomes} have a tie across biome ids — a difference at a unique minimum \
         means `nearest_row_vanilla_exact` is mis-ported"
    );

    // **A regular lattice inflates exact ties.** Every coordinate above is a
    // multiple of 2,750, so many rows sit at symmetric distances and the squared
    // sums collide far more often than for an arbitrary integer. Real climate
    // targets are `(f32 * 10000) as i64` — effectively arbitrary 5-digit integers
    // — so the same rates measured on non-round targets are the ones that bound
    // the production risk. Measured here rather than argued.
    let arbitrary = base_points(200_000);
    let mut arb_ours = 0usize;
    let mut arb_vanilla_biome = 0usize;
    let mut arb_tied_across_biomes = 0usize;
    for target in &arbitrary {
        let brute = nearest_row_brute_force(&rows, target);
        if table.nearest_row(target) != brute {
            arb_ours += 1;
        }
        let vanilla = table.nearest_row_vanilla_exact(target, None);
        if vanilla != brute && table.biome_at(vanilla) != table.biome_at(brute) {
            arb_vanilla_biome += 1;
        }
        let best = rows[brute as usize].fitness(target);
        let tied: Vec<&BiomeParameterPoint> =
            rows.iter().filter(|r| r.fitness(target) == best).collect();
        if tied.len() > 1 && tied.iter().any(|r| r.biome != tied[0].biome) {
            arb_tied_across_biomes += 1;
        }
    }
    println!(
        "{} arbitrary (non-round) targets:\n  ours-vs-brute:            {} disagreements\n  \
         vanilla's-tree-vs-brute:  {} by biome id\n  tied across biome ids:    {}",
        arbitrary.len(),
        arb_ours,
        arb_vanilla_biome,
        arb_tied_across_biomes
    );
    assert_eq!(
        arb_ours, 0,
        "our search must agree with brute force on arbitrary targets too"
    );
    assert!(
        arb_vanilla_biome <= arb_tied_across_biomes,
        "same self-check on the arbitrary arm: {arb_vanilla_biome} differences against \
         {arb_tied_across_biomes} ties"
    );
}
