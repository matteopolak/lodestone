//! Per-wrapper fixtures for the five semantics a flattened density graph can
//! silently break.
//!
//! `chunk_parity` proves the whole field against a JVM dump, which is the
//! strongest evidence available — but it is one router at one seed, so it can
//! only exercise the semantics that router happens to use, and it reports a
//! single percentage when it fails. These fixtures are the per-semantic
//! complement: each isolates one rule, and each carries a **control** showing the
//! assertion could have failed.
//!
//! | # | semantic | observable here as |
//! |---|---|---|
//! | 1 | `mul`'s `v1 == 0.0` short-circuit | `0.0` vs `NaN` |
//! | 2 | `interpolated`-inside-corner transparency | transparent value vs bilinear blend |
//! | 3 | `flat_cache`'s quart snap and forced `y = 0` | equality classes over `(x, z)`, independence from `y` |
//! | 4 | `cache_2d` / `cache_once` scoping | the two evaluators deliberately disagreeing |
//! | 5 | `cache_all_in_cell` selecting vanilla's own math-helper trilinear lerp | X-inner vs Y-inner nesting |
//!
//! # Why `cell_width = 8` for most of them
//!
//! This is the load-bearing detail of the whole file, and it is the reason a
//! naive version of these tests passes while measuring nothing.
//!
//! `flat_cache` snaps XZ to the **quart** grid — a hardcoded `>> 2 << 2` in
//! vanilla, *not* `cell_width`. When `cell_width` is also 4, as it is for the
//! overworld, every quart-snapped position and every corner position is a cell
//! corner. And at a cell corner all three lerp factors are exactly `0.0`, where
//! `lerp(0.0, a, b) == a` exactly — so an `interpolated` node evaluated there is
//! the identity whether or not it is transparent, and the X-inner and Y-inner
//! nestings agree bit-for-bit. **At `cell_width = 4` semantics 2 and 5 are
//! value-unobservable at every position the evaluator ever evaluates them.**
//!
//! Setting `cell_width = 8` puts the quart grid *mid-cell*, which is what makes
//! the difference reachable. `cell_width` is a noise-settings parameter and none
//! of these rules is specific to 4, so this is a legitimate configuration rather
//! than a contrivance — but it does mean these two semantics are **not** covered
//! by the overworld router, and `chunk_parity` passing says nothing about them.
//! That is stated here rather than left for someone to discover.
//!
//! A related honesty note on semantic 2: the compiled overworld `final_density`
//! has five `interpolated` nodes and **none** of them is nested inside another
//! (`Program::interpolating_slots` finds all five reachable in an interpolating
//! context). Only two are entered per block, and that is `mul` short-circuiting
//! and `range_choice` branching, *not* transparency. So the real router does not
//! exercise semantic 2 at all, in either direction.

use std::collections::HashMap;

use lodestone_worldgen::density::{Builder, Context, NoiseChunkSampler, NoiseParams, Resolver};
use lodestone_worldgen::math;
use serde_json::{Value, json};

const SEED: i64 = 42;

/// An in-memory resolver: enough to instantiate a real `NormalNoise` so fixtures
/// can contain a function that genuinely varies in all three axes. A `Const` or
/// `y_clamped_gradient` fixture cannot expose an interpolation-order difference,
/// because a function of `y` alone has x/z-invariant corners and every nesting
/// of the lerps then agrees exactly.
struct MemResolver {
    functions: HashMap<String, Value>,
}

impl Resolver for MemResolver {
    fn density_function(&self, id: &str) -> Value {
        self.functions
            .get(id)
            .cloned()
            .unwrap_or_else(|| panic!("no fixture density function {id}"))
    }
    fn noise(&self, _id: &str) -> NoiseParams {
        // Shape mirrors a real router noise (several octaves, negative first
        // octave) so the field is smooth but not linear.
        NoiseParams {
            first_octave: -7,
            amplitudes: vec![1.0, 1.0, 1.0, 1.0],
        }
    }
}

fn resolver() -> MemResolver {
    MemResolver {
        functions: HashMap::new(),
    }
}

/// A 3-D noise node, the fixture building block that varies in every axis.
fn noise3() -> Value {
    json!({"type": "minecraft:noise", "noise": "minecraft:fixture", "xz_scale": 1.0, "y_scale": 1.0})
}

// =========================================================================
// Semantic 1 — mul's `v1 == 0.0` short-circuit
// =========================================================================

/// `mul` must not evaluate its second operand when the first is exactly `0.0`.
///
/// Made observable by putting an **infinity** in the second operand: `0.0 * inf`
/// is `NaN`, so a graph that evaluated both operands produces `NaN` where the
/// short-circuit produces a clean `0.0`. This is a value difference, not a
/// counter difference, so it holds with the counters compiled out.
#[test]
fn mul_does_not_evaluate_its_second_operand_when_the_first_is_zero() {
    let r = resolver();
    let b = Builder::new(SEED, &r);

    // `invert` of 0.0 is 1/0 = +inf.
    let poison = json!({"type": "minecraft:invert", "argument": 0.0});

    let short = b
        .build(&json!({
            "type": "minecraft:mul", "argument1": 0.0, "argument2": poison
        }))
        .expect("test fixture density-function document");
    let sampler = NoiseChunkSampler::new(short, b.slot_count(), 4, 8);
    let got = sampler.final_density(1, 2, 3);
    assert_eq!(
        got, 0.0,
        "mul(0.0, 1/0) returned {got}; NaN means both operands were evaluated \
         and the short-circuit is gone. A flattened graph swept bottom-up would \
         fail exactly here."
    );
    assert!(!got.is_nan(), "mul(0.0, 1/0) must not be NaN");

    // Control: the poison operand really is infinite, so the assertion above had
    // something to catch. Without this the test would pass just as happily
    // against `argument2: 0.0`.
    let b2 = Builder::new(SEED, &r);
    let live = b2
        .build(&json!({
            "type": "minecraft:mul", "argument1": 1.0,
            "argument2": {"type": "minecraft:invert", "argument": 0.0}
        }))
        .expect("test fixture density-function document");
    let control = NoiseChunkSampler::new(live, b2.slot_count(), 4, 8).final_density(1, 2, 3);
    assert!(
        control.is_infinite(),
        "control: mul(1.0, 1/0) should be infinite, got {control} — the poison \
         operand is not poisonous and the short-circuit test proves nothing"
    );

    // And the point interpreter must short-circuit identically; it is a separate
    // `match` over the same tree and has diverged before.
    let b3 = Builder::new(SEED, &r);
    let tree = b3
        .build(&json!({
            "type": "minecraft:mul", "argument1": 0.0,
            "argument2": {"type": "minecraft:invert", "argument": 0.0}
        }))
        .expect("test fixture density-function document");
    assert_eq!(
        tree.compute(Context::new(1, 2, 3)),
        0.0,
        "the point interpreter lost the short-circuit"
    );
}

// =========================================================================
// Semantics 2 + 3 — nested-interpolated transparency, and flat_cache's snap
// =========================================================================

/// `flat_cache` evaluates its inner at the quart-snapped position with
/// `y = 0` **and** with `interpolate = false`, so a nested `interpolated` is
/// transparent there.
///
/// `cell_width = 8` puts the quart grid mid-cell, which is the only way this is
/// observable — see the module doc. The two hypotheses are computed here from the
/// *point* interpreter (which is transparent for both wrappers) and the
/// measurement is required to land on one:
///
/// * **transparent**: the value is the raw inner at the snapped position.
/// * **not transparent**: the nested `interpolated` would lerp between its own
///   cell corners at the snapped position, giving a blend.
#[test]
fn flat_cache_evaluates_a_nested_interpolated_transparently() {
    let r = resolver();
    let b = Builder::new(SEED, &r);
    const CW: i32 = 8;
    const CH: i32 = 8;

    // flat_cache(interpolated(noise))
    let tree = b
        .build(&json!({
            "type": "minecraft:flat_cache",
            "argument": {"type": "minecraft:interpolated", "argument": noise3()}
        }))
        .expect("test fixture density-function document");
    let sampler = NoiseChunkSampler::new(tree, b.slot_count(), CW, CH);

    // A raw-noise oracle over the same instantiated noise, via the point
    // interpreter (transparent for flat_cache and interpolated alike).
    let b2 = Builder::new(SEED, &r);
    let raw = b2.build(&noise3()).expect("test fixture density-function document");
    let at = |x: i32, y: i32, z: i32| raw.compute(Context::new(x, y, z));

    // Query at (5, 3, 5): quart snap gives (4, 0, 4), which with cell_width 8 is
    // the centre of the cell spanning x,z in [0, 8) — mid-cell in X and Z.
    let got = sampler.final_density(5, 3, 5);

    let transparent = at(4, 0, 4);
    // The wrong hypothesis: a non-transparent nested `interpolated` at (4, 0, 4)
    // lerps its own 8 corners with fx = fz = 0.5, fy = 0.
    let blended = math::lerp3(
        0.5,
        0.0,
        0.5,
        at(0, 0, 0),
        at(8, 0, 0),
        at(0, 8, 0),
        at(8, 8, 0),
        at(0, 0, 8),
        at(8, 0, 8),
        at(0, 8, 8),
        at(8, 8, 8),
    );

    // Anti-vacuity: the two hypotheses must actually differ, or landing on one
    // says nothing. This is what a `cell_width = 4` version of this test would
    // fail, and it would fail *silently* as a pass.
    assert_ne!(
        transparent.to_bits(),
        blended.to_bits(),
        "the transparent and blended hypotheses coincide ({transparent}), so this \
         fixture cannot distinguish them — check cell_width is not 4"
    );
    assert_eq!(
        got.to_bits(),
        transparent.to_bits(),
        "flat_cache(interpolated(noise)) at (5,3,5) gave {got}; transparent \
         hypothesis {transparent}, blended hypothesis {blended}. Landing on the \
         blend means the `interpolate = false` flag stopped threading through \
         flat_cache's inner."
    );
}

/// `flat_cache` keys on the quart grid and forces `y = 0`: positions sharing a
/// `4 × 4` XZ cell agree for **every** `y`, and adjacent quart cells differ.
#[test]
fn flat_cache_snaps_xz_to_the_quart_grid_and_ignores_y() {
    let r = resolver();
    let b = Builder::new(SEED, &r);
    let tree = b
        .build(&json!({"type": "minecraft:flat_cache", "argument": noise3()}))
        .expect("test fixture density-function document");
    let s = NoiseChunkSampler::new(tree, b.slot_count(), 4, 8);

    let base = s.final_density(4, 0, 4);
    // Same quart cell, wildly different y — the forced `y = 0`.
    for (x, y, z) in [(4, 0, 4), (5, 7, 5), (7, -64, 7), (6, 319, 6), (4, 123, 7)] {
        assert_eq!(
            s.final_density(x, y, z).to_bits(),
            base.to_bits(),
            "({x},{y},{z}) shares the quart cell of (4,0,4) and must give the \
             same value; a difference means the snap or the forced y = 0 is gone"
        );
    }
    // Adjacent quart cells must differ, or the "same value" assertions above are
    // satisfied by a constant.
    let neighbour = s.final_density(8, 0, 4);
    assert_ne!(
        neighbour.to_bits(),
        base.to_bits(),
        "control: the next quart cell in X returned the same value, so the \
         equality assertions above prove nothing"
    );

    // And the y-independence is a property of flat_cache, not of the noise: the
    // same noise sampled directly does vary with y.
    let b2 = Builder::new(SEED, &r);
    let raw = b2.build(&noise3()).expect("test fixture density-function document");
    assert_ne!(
        raw.compute(Context::new(4, 0, 4)).to_bits(),
        raw.compute(Context::new(4, 7, 4)).to_bits(),
        "control: the fixture noise does not vary with y, so `ignores y` is \
         vacuous"
    );
}

// =========================================================================
// Semantic 4 — cache_2d / cache_once scoping
// =========================================================================

/// `cache_2d` is **transparent in both evaluators**, which is what vanilla's
/// unwrapped marker-node compute does
/// (`return this.wrapped.compute(context);`).
///
/// This gate used to assert the opposite for the point interpreter, because that
/// interpreter carried a single-slot last-`(x, z)` memo. §12.132 retired it: over a
/// 289-column burst the memo hit **0.12%** of its 19.9M lookups, and the 708 copies
/// of it inside the `Arc`-shared graph collapsed IPC from 5.46 to 1.32 under a
/// 20-column generation window. The fixture below is the same y-dependent subtree
/// as before — deliberately violating `cache_2d`'s own contract, which is what
/// makes any memo *visible* — so it is still the strongest available detector of
/// one being reintroduced, only with the expectation flipped.
#[test]
fn cache_2d_is_transparent_in_both_evaluators() {
    let r = resolver();
    let b = Builder::new(SEED, &r);
    let tree = b
        .build(&json!({"type": "minecraft:cache_2d", "argument": noise3()}))
        .expect("test fixture density-function document");

    // Field: transparent, so it must track y.
    let s = NoiseChunkSampler::new(tree.clone(), b.slot_count(), 4, 8);
    let f0 = s.final_density(3, 0, 3);
    let f1 = s.final_density(3, 40, 3);
    assert_ne!(
        f0.to_bits(),
        f1.to_bits(),
        "cache_2d must be transparent in the block field, but the value did not \
         change with y ({f0} at y=0, {f1} at y=40) — it is memoising on (x, z) \
         where vanilla's NoiseChunk does not"
    );

    // Point interpreter: also transparent, so the same (x, z) at a different y must
    // track y rather than return a memoised first value. A reintroduced memo fails
    // exactly here, and it can only be seen because the fixture's subtree depends on
    // y — which a real cache_2d's never does (every one in 26.2's shipped data wraps
    // shift_a / blend_alpha / a spline over continents, all xz-only).
    let p0 = tree.compute(Context::new(3, 0, 3));
    let p1 = tree.compute(Context::new(3, 40, 3));
    assert_ne!(
        p1.to_bits(),
        p0.to_bits(),
        "cache_2d must be transparent in the point interpreter, but y=40 at the same \
         (x, z) returned the y=0 value {p0} — a last-(x, z) memo is back"
    );
    // Control: the field and point evaluators must now *agree*, which is the
    // positive half. Without it, "tracks y" is satisfied by any two different
    // numbers, including a broken evaluator.
    assert_eq!(
        p0.to_bits(),
        f0.to_bits(),
        "the two evaluators disagree at (3, 0, 3): point {p0} against field {f0}. \
         cache_2d is transparent in both, so they must be bit-identical"
    );
    assert_eq!(
        p1.to_bits(),
        f1.to_bits(),
        "the two evaluators disagree at (3, 40, 3): point {p1} against field {f1}"
    );
}

/// `cache_once`, `cache_all_in_cell` and `blend_density` are value-transparent in
/// **both** evaluators. Vanilla's `CacheOnce`/`CacheAllInCell` both check
/// `context != NoiseChunk.this` and fall through to an uncached `compute`, which
/// is always true for a single-point context; and `blend_density` only does
/// anything with a non-empty blender, which this crate never constructs.
#[test]
fn cache_once_and_friends_are_transparent_in_both_evaluators() {
    let r = resolver();
    for ty in [
        "minecraft:cache_once",
        "minecraft:cache_all_in_cell",
        "minecraft:blend_density",
    ] {
        let b = Builder::new(SEED, &r);
        let wrapped = b
            .build(&json!({"type": ty, "argument": noise3()}))
            .expect("test fixture density-function document");
        let bare = Builder::new(SEED, &r);
        let plain = bare.build(&noise3()).expect("test fixture density-function document");

        let ws = NoiseChunkSampler::new(wrapped.clone(), b.slot_count(), 4, 8);
        let ps = NoiseChunkSampler::new(plain.clone(), bare.slot_count(), 4, 8);

        let mut varied = 0;
        let mut last: Option<u64> = None;
        for (x, y, z) in [(1, 2, 3), (1, 40, 3), (9, 40, 11), (-5, -60, 7)] {
            let w = ws.final_density(x, y, z);
            let p = ps.final_density(x, y, z);
            assert_eq!(
                w.to_bits(),
                p.to_bits(),
                "{ty} changed the block-field value at ({x},{y},{z}): {w} vs {p}"
            );
            assert_eq!(
                wrapped.compute(Context::new(x, y, z)).to_bits(),
                plain.compute(Context::new(x, y, z)).to_bits(),
                "{ty} changed the point value at ({x},{y},{z})"
            );
            if last.is_some_and(|l| l != w.to_bits()) {
                varied += 1;
            }
            last = Some(w.to_bits());
        }
        // Control: "identical to unwrapped" is trivially true of a constant.
        assert!(
            varied > 0,
            "control: {ty}'s fixture returned the same value everywhere, so the \
             transparency assertions are vacuous"
        );
    }
}

// =========================================================================
// Semantic 5 — cache_all_in_cell selects vanilla's own math-helper trilinear lerp (X inner)
// =========================================================================

/// The block field interpolates in vanilla's own math-helper trilinear-lerp nesting — **X inner**, then
/// Y, then Z — not the incremental `updateForY → X → Z` chain, which is Y inner.
///
/// `tests/interpolation_order.rs` is the standing guard for this on the *real
/// router*; this is the engine-level fixture, and it exists because the two are
/// only distinguishable at a position where no lerp factor is zero. Both nestings
/// are computed here from corner values obtained through the point interpreter,
/// and the measurement must equal one and differ from the other.
#[test]
fn the_field_interpolates_x_inner_not_y_inner() {
    let r = resolver();
    const CW: i32 = 8;
    const CH: i32 = 8;

    let b = Builder::new(SEED, &r);
    let tree = b
        .build(&json!({"type": "minecraft:interpolated", "argument": noise3()}))
        .expect("test fixture density-function document");
    let s = NoiseChunkSampler::new(tree, b.slot_count(), CW, CH);

    let b2 = Builder::new(SEED, &r);
    let raw = b2.build(&noise3()).expect("test fixture density-function document");
    let at = |x: i32, y: i32, z: i32| raw.compute(Context::new(x, y, z));

    let mut checked = 0usize;
    let mut distinguishing = 0usize;
    for (px, py, pz) in [
        (3i32, 5i32, 6i32),
        (1, 7, 2),
        (5, 1, 3),
        (11, 13, 6),
        (-3, -5, -6),
        (21, 29, 13),
    ] {
        let (cx, cy, cz) = (
            px.div_euclid(CW) * CW,
            py.div_euclid(CH) * CH,
            pz.div_euclid(CW) * CW,
        );
        let (x1, y1, z1) = (cx + CW, cy + CH, cz + CW);
        let n = [
            at(cx, cy, cz),
            at(x1, cy, cz),
            at(cx, y1, cz),
            at(x1, y1, cz),
            at(cx, cy, z1),
            at(x1, cy, z1),
            at(cx, y1, z1),
            at(x1, y1, z1),
        ];
        let fx = f64::from(px.rem_euclid(CW)) / f64::from(CW);
        let fy = f64::from(py.rem_euclid(CH)) / f64::from(CH);
        let fz = f64::from(pz.rem_euclid(CW)) / f64::from(CW);
        assert!(
            fx != 0.0 && fy != 0.0 && fz != 0.0,
            "({px},{py},{pz}) sits on a cell boundary, where every nesting agrees \
             and the comparison is a tautology"
        );

        // X inner: lerp over x, then y, then z. This is vanilla's own math-helper trilinear lerp.
        let x_inner = math::lerp3(fx, fy, fz, n[0], n[1], n[2], n[3], n[4], n[5], n[6], n[7]);
        // Y inner: lerp over y first, then x, then z — the incremental chain's
        // order. Algebraically identical, a different IEEE 754 expression.
        let l = |t: f64, a: f64, b: f64| a + t * (b - a);
        let y_inner = {
            let y00 = l(fy, n[0], n[2]);
            let y10 = l(fy, n[1], n[3]);
            let y01 = l(fy, n[4], n[6]);
            let y11 = l(fy, n[5], n[7]);
            l(fz, l(fx, y00, y10), l(fx, y01, y11))
        };

        let got = s.final_density(px, py, pz);
        assert_eq!(
            got.to_bits(),
            x_inner.to_bits(),
            "at ({px},{py},{pz}) the field gave {got}; vanilla's own math-helper trilinear lerp (X inner) is \
             {x_inner} and the incremental chain (Y inner) is {y_inner}. Landing \
             on the Y-inner value is the 90563/98304 failure, which reads as a \
             tolerance problem rather than a wrong algorithm."
        );
        checked += 1;
        if x_inner.to_bits() != y_inner.to_bits() {
            distinguishing += 1;
        }
    }

    assert_eq!(checked, 6, "all six positions must be checked");
    // The one that makes this test non-vacuous: at least one position where the
    // two nestings genuinely produce different bits. Without this the whole test
    // is satisfied by any correct-up-to-reassociation implementation, which is
    // precisely the bug it exists to catch.
    assert!(
        distinguishing > 0,
        "no sampled position distinguished the X-inner and Y-inner nestings, so \
         this test cannot tell a correct port from a wrong one. Add positions or \
         change the fixture noise; do not delete the assertion."
    );
    println!(
        "engine interpolation order: {checked} positions checked, \
         {distinguishing} bit-distinguishable between the two nestings"
    );
}
