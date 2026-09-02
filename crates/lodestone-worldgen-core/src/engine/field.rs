//! The block-field evaluator: vanilla's `NoiseChunk` semantics over a flattened
//! [`Graph`].
//!
//! This is a **recursive descent over indices**, not a bottom-up sweep over the
//! `Vec<Op>`, and that is forced rather than stylistic — see
//! [`Field::eval`]'s `Mul` arm. Three other node kinds also branch
//! (`range_choice`, `interval_select`, and `interpolated`'s two regimes), so the
//! set of nodes an evaluation touches is position-dependent and cannot be known
//! before walking.
//!
//! # The five semantics this file has to preserve
//!
//! Each is a place a flattening can silently differ, so each is called out at
//! the arm that implements it and gated by its own fixture in
//! `crates/lodestone-worldgen/tests/engine_semantics.rs`:
//!
//! 1. **`Mul`'s `v1 == 0.0` short-circuit** — the second operand is not
//!    evaluated at all.
//! 2. **`interpolated`-inside-corner transparency** — while filling a corner,
//!    a nested `interpolated` is transparent.
//! 3. **`flat_cache`'s quart snap and forced `y = 0`**.
//! 4. **`cache_2d` / `cache_once` scoping** — transparent *here*; `cache_2d` is
//!    a real memo only in the point interpreter.
//! 5. **`cache_all_in_cell`** — transparent as a value, but it is what selects
//!    `Mth.lerp3` (X-inner) over the incremental chain (Y-inner) for the
//!    interpolation. See [`Field::interpolate`].
//!
//! # Float order
//!
//! Every operation here is IEEE-exact: `+`, `-`, `*`, `/`, `min`, `max`, `abs`,
//! `clamp`, and the `Mth.lerp*` family built from them. No `mul_add`, no FMA, no
//! reassociation, and no transcendental — so unlike the noise-init constants
//! (`crate::math::exp2_exact`) there is no 1-ulp question in the field walk at
//! all.

use super::graph::{Graph, NodeId, OpKind};
use super::scratch::Scratch;
use crate::density::Context;

/// Cell geometry: `NoiseSettings.getCellWidth()/getCellHeight()` — 4 and 8 for
/// the overworld.
#[derive(Clone, Copy, Debug)]
pub struct Geom {
    /// Vanilla's own cell-width field, the X/Z cell edge.
    pub cell_width: i32,
    /// Vanilla's own cell-height field, the Y cell edge.
    pub cell_height: i32,
}

/// One evaluation in flight: an immutable graph, the geometry, and the mutable
/// per-chunk scratch.
///
/// Holding the `&mut Scratch` for the whole descent is the point: the old
/// tree walker took a `RefCell` borrow per corner lookup, and there are
/// hundreds of thousands of those per chunk.
pub(crate) struct Field<'a> {
    graph: &'a Graph,
    geom: Geom,
    scratch: &'a mut Scratch,
}

impl<'a> Field<'a> {
    pub(crate) fn new(graph: &'a Graph, geom: Geom, scratch: &'a mut Scratch) -> Self {
        Self {
            graph,
            geom,
            scratch,
        }
    }

    /// Evaluates `id` at a block through the `NoiseChunk` wrapping.
    pub(crate) fn eval(&mut self, id: NodeId, x: i32, y: i32, z: i32, interpolate: bool) -> f64 {
        let op = self.graph.op(id);
        crate::counters::bump_density_eval(op.kind as usize);
        super::redundancy_probe::visit_field(
            std::ptr::from_ref(self.graph).cast::<()>(),
            id,
            op.kind as usize,
            x,
            y,
            z,
            self.scratch.probe_scope(),
        );
        match op.kind {
            OpKind::Const => self.graph.param(op.a),
            OpKind::BlendAlpha => 1.0,
            OpKind::BlendOffset | OpKind::Beardifier => 0.0,
            OpKind::YClampedGradient => {
                let p = op.a;
                crate::math::clamped_map(
                    f64::from(y),
                    self.graph.param(p),
                    self.graph.param(p + 1),
                    self.graph.param(p + 2),
                    self.graph.param(p + 3),
                )
            }

            OpKind::Add => {
                self.eval(op.a, x, y, z, interpolate) + self.eval(op.b, x, y, z, interpolate)
            }
            // Semantic 1. Vanilla does not evaluate the second operand when the
            // first is exactly zero. This is why the evaluator is a recursive
            // descent: a bottom-up sweep over `ops` would evaluate every node,
            // which is not merely slower — a skipped subtree can contain a
            // `flat_cache`/`interpolated` slot write, so evaluating it would
            // populate caches vanilla leaves empty and change what a *later*
            // query returns.
            OpKind::Mul => {
                let v1 = self.eval(op.a, x, y, z, interpolate);
                if v1 == 0.0 {
                    0.0
                } else {
                    v1 * self.eval(op.b, x, y, z, interpolate)
                }
            }
            OpKind::Min => self
                .eval(op.a, x, y, z, interpolate)
                .min(self.eval(op.b, x, y, z, interpolate)),
            OpKind::Max => self
                .eval(op.a, x, y, z, interpolate)
                .max(self.eval(op.b, x, y, z, interpolate)),

            OpKind::Abs => self.eval(op.a, x, y, z, interpolate).abs(),
            OpKind::Square => {
                let v = self.eval(op.a, x, y, z, interpolate);
                v * v
            }
            OpKind::Cube => {
                let v = self.eval(op.a, x, y, z, interpolate);
                v * v * v
            }
            OpKind::HalfNegative => {
                let v = self.eval(op.a, x, y, z, interpolate);
                if v > 0.0 { v } else { v * 0.5 }
            }
            OpKind::QuarterNegative => {
                let v = self.eval(op.a, x, y, z, interpolate);
                if v > 0.0 { v } else { v * 0.25 }
            }
            OpKind::Squeeze => {
                let c = self.eval(op.a, x, y, z, interpolate).clamp(-1.0, 1.0);
                c / 2.0 - c * c * c / 24.0
            }
            OpKind::Invert => 1.0 / self.eval(op.a, x, y, z, interpolate),
            OpKind::Clamp => {
                let v = self.eval(op.a, x, y, z, interpolate);
                v.clamp(self.graph.param(op.b), self.graph.param(op.b + 1))
            }

            OpKind::Interpolated => {
                if interpolate {
                    self.interpolate(op.a, op.b as usize, x, y, z)
                } else {
                    // Semantic 2. Nested inside a corner sample: vanilla's
                    // interpolator is transparent when the context is not the
                    // NoiseChunk itself. The flag has to thread through the
                    // descent, which is the only reason `eval` carries it.
                    self.eval(op.a, x, y, z, false)
                }
            }
            // Semantic 3. XZ snapped to the quart grid (multiples of 4,
            // hardcoded in vanilla — deliberately *not* `cell_width`) and `y`
            // forced to exactly 0, so a 2D climate/shift field is sampled once
            // per 4x4 column. The inner is evaluated with `interpolate = false`.
            OpKind::FlatCache => {
                let qx = (x >> 2) << 2;
                let qz = (z >> 2) << 2;
                self.slot_get(op.b as usize, (qx, 0, qz), op.a)
            }
            // Semantic 4. `cache_2d` is a real last-(x, z) memo in the *point*
            // interpreter and transparent here; `cache_once`,
            // `cache_all_in_cell` and `blend_density` (all compiled to Marker)
            // are transparent in both. The node is still walked so the
            // per-kind `density_evals` counter keeps reporting it.
            OpKind::Cache2D | OpKind::Marker => self.eval(op.a, x, y, z, interpolate),

            // Memoised on the last `(x, y, z)` — see `Scratch::leaf_get`. Safe
            // here and at the leaf arm below, and at no other arm, because these
            // are the only kinds with no field children: nothing under them can
            // write a cache slot a later query depends on.
            OpKind::Noise => {
                if let Some(v) = self.scratch.leaf_get(id, x, y, z) {
                    return v;
                }
                let n = self.graph.noise(op.a);
                let xz = self.graph.param(op.b);
                let ys = self.graph.param(op.b + 1);
                let v = n.get_value(f64::from(x) * xz, f64::from(y) * ys, f64::from(z) * xz);
                self.scratch.leaf_put(id, x, y, z, v);
                v
            }
            OpKind::ShiftedNoise => {
                let (c0, c1, c2) = (
                    self.graph.child(op.a),
                    self.graph.child(op.a + 1),
                    self.graph.child(op.a + 2),
                );
                let xz = self.graph.param(op.c);
                let ys = self.graph.param(op.c + 1);
                let sx = f64::from(x) * xz + self.eval(c0, x, y, z, interpolate);
                let sy = f64::from(y) * ys + self.eval(c1, x, y, z, interpolate);
                let sz = f64::from(z) * xz + self.eval(c2, x, y, z, interpolate);
                self.graph.noise(op.b).get_value(sx, sy, sz)
            }
            OpKind::ShiftA => {
                shift(self.graph.noise(op.a), f64::from(x), 0.0, f64::from(z))
            }
            OpKind::ShiftB => {
                shift(self.graph.noise(op.a), f64::from(z), f64::from(x), 0.0)
            }
            OpKind::Shift => shift(
                self.graph.noise(op.a),
                f64::from(x),
                f64::from(y),
                f64::from(z),
            ),

            OpKind::RangeChoice => {
                let input = self.graph.child(op.a);
                let v = self.eval(input, x, y, z, interpolate);
                let lo = self.graph.param(op.b);
                let hi = self.graph.param(op.b + 1);
                let branch = if v >= lo && v < hi {
                    self.graph.child(op.a + 1)
                } else {
                    self.graph.child(op.a + 2)
                };
                self.eval(branch, x, y, z, interpolate)
            }
            // Layout is `children[a] = n`, `children[a + 1] = input`,
            // `children[a + 2 + i] = functions[i]`; `b` = thresholds offset,
            // `c` = threshold count. The loop is over the *threshold* count,
            // matching the tree walker's `thresholds.iter()`, not over the
            // function count — see the compiler's note on this arm for why the
            // two are stored separately.
            OpKind::IntervalSelect => {
                let n = self.graph.child(op.a);
                let input = self.graph.child(op.a + 1);
                let v = self.eval(input, x, y, z, interpolate);
                for i in 0..op.c {
                    if v < self.graph.param(op.b + i) {
                        let branch = self.graph.child(op.a + 2 + i);
                        return self.eval(branch, x, y, z, interpolate);
                    }
                }
                let branch = self.graph.child(op.a + 1 + n);
                self.eval(branch, x, y, z, interpolate)
            }

            // The three point-evaluated leaves. The field evaluator does not
            // recurse into these; it calls the point interpreter, so everything
            // beneath one of them has point semantics (no quart snapping, no
            // interpolation). That is a real vanilla semantic — see
            // `super::graph`'s module doc — and it is why these hold an
            // untouched `Density` subtree rather than compiled nodes.
            //
            // Memoised on the last `(x, y, z)`: `old_blended_noise` is reached
            // twice per corner evaluation (one DAG node, two parents) and one
            // `BlendedNoise::sample` is up to 40 `ImprovedNoise::noise_scaled`
            // calls, the most expensive kernel in the engine. `Scratch::leaf_get`
            // carries the measurement and the safety argument.
            OpKind::Spline | OpKind::Blended | OpKind::FindTopSurface | OpKind::EndIslands => {
                if let Some(v) = self.scratch.leaf_get(id, x, y, z) {
                    return v;
                }
                let v = self.graph.leaf(op.a).compute(Context::new(x, y, z));
                self.scratch.leaf_put(id, x, y, z, v);
                v
            }
        }
    }

    /// `interpolated`: eight corners of the enclosing
    /// `cell_width × cell_height × cell_width` cell, trilinearly interpolated.
    ///
    /// # The corner hoist
    ///
    /// The eight corner values are cached **per cell**, not fetched per block.
    /// A chunk has 98,304 blocks but only 768 cells, so this is the difference
    /// between `98_304 × 8 = 786_432` corner lookups and `768 × 8 = 6_144`.
    /// The corners themselves are still fetched through
    /// [`Self::slot_get`], because adjacent cells *share* corners and the true
    /// number of distinct corner evaluations is `5 × 49 × 5 = 1_225` — dropping
    /// that second layer would multiply the expensive half of the work by five.
    /// This is vanilla's `CacheAllInCell` hoist, holding the eight corners
    /// rather than the 128 interpolated values, which costs the same arithmetic
    /// and a 128th of the memory.
    ///
    /// # Semantic 5: which interpolation order
    ///
    /// `Mth.lerp3` — **X inner**, then Y, then Z. Vanilla's `NoiseInterpolator`
    /// computes the same eight corners two ways and they are different IEEE 754
    /// expressions; the incremental Y-then-X-then-Z update chain is Y-inner and
    /// is **not** what `final_density` reads, because vanilla's own per-chunk
    /// field's constructor
    /// wraps the router in a code-only cache-all-in-cell wrapper whose array
    /// is filled
    /// while its own "filling cell" flag is true. Swapping this to the incremental chain takes
    /// the whole-chunk JVM gate from 98304/98304 to 90563/98304 — a 92% pass
    /// rate, which reads like a tolerance problem rather than a wrong algorithm.
    /// `docs/worldgen-density-engine.md` has the full account and
    /// `tests/interpolation_order.rs` is the standing guard.
    fn interpolate(&mut self, inner: NodeId, slot: usize, x: i32, y: i32, z: i32) -> f64 {
        let cw = self.geom.cell_width;
        let ch = self.geom.cell_height;
        let (cx, cy, cz) = (x.div_euclid(cw), y.div_euclid(ch), z.div_euclid(cw));

        let n = match self.scratch.cell_get(slot, cx, cy, cz) {
            Some(v) => v,
            None => {
                crate::counters::bump_cell_fill();
                let (x0, y0, z0) = (cx * cw, cy * ch, cz * cw);
                let (x1, y1, z1) = (x0 + cw, y0 + ch, z0 + cw);
                // Fetch order kept identical to the pre-flattening walker so
                // the slot hit/miss counter populations stay comparable.
                let v = [
                    self.corner(inner, slot, x0, y0, z0),
                    self.corner(inner, slot, x1, y0, z0),
                    self.corner(inner, slot, x0, y1, z0),
                    self.corner(inner, slot, x1, y1, z0),
                    self.corner(inner, slot, x0, y0, z1),
                    self.corner(inner, slot, x1, y0, z1),
                    self.corner(inner, slot, x0, y1, z1),
                    self.corner(inner, slot, x1, y1, z1),
                ];
                self.scratch.cell_put(slot, cx, cy, cz, v);
                v
            }
        };

        let fx = f64::from(x.rem_euclid(cw)) / f64::from(cw);
        let fy = f64::from(y.rem_euclid(ch)) / f64::from(ch);
        let fz = f64::from(z.rem_euclid(cw)) / f64::from(cw);

        crate::math::lerp3(fx, fy, fz, n[0], n[1], n[2], n[3], n[4], n[5], n[6], n[7])
    }

    /// One corner fetch. Counted separately from [`Self::slot_get`]'s own
    /// hit/miss because this must stay "corner lookups" exactly — 8 per *cell
    /// fill* after the hoist, hit or miss — which is the number the geometry
    /// predicts and the bench asserts.
    /// One corner fetch, with the memo lookup inlined rather than delegated to
    /// [`Self::slot_get`].
    ///
    /// The duplication is deliberate and is about the counters: `corner_evals`
    /// must count corner evaluations *only*, because that is the quantity with a
    /// derived prediction (the `5 × 49 × 5` lattice) and the one the hoist must
    /// leave unchanged — while `slot_get` is also `flat_cache`'s path, whose
    /// misses belong to a different population. Doing it by probing through
    /// `slot_get` first would have cost a second lookup on the hottest path in
    /// the engine for the sake of a counter that is compiled out by default.
    fn corner(&mut self, inner: NodeId, slot: usize, x: i32, y: i32, z: i32) -> f64 {
        crate::counters::bump_corner_lookup();
        let key = (x, y, z);
        if let Some(v) = self.scratch.slot_get(slot, key) {
            crate::counters::bump_slot_hit();
            return v;
        }
        crate::counters::bump_slot_miss(slot);
        crate::counters::bump_corner_eval();
        let v = self.eval(inner, x, y, z, false);
        self.scratch.slot_put(slot, key, v);
        v
    }

    /// A memoised evaluation of `inner` at an exact key, shared by
    /// `interpolated` corners and `flat_cache`.
    ///
    /// The inner is always evaluated with `interpolate = false`: a corner sample
    /// is not itself an interpolation context.
    fn slot_get(&mut self, slot: usize, key: (i32, i32, i32), inner: NodeId) -> f64 {
        if let Some(v) = self.scratch.slot_get(slot, key) {
            crate::counters::bump_slot_hit();
            return v;
        }
        crate::counters::bump_slot_miss(slot);
        let v = self.eval(inner, key.0, key.1, key.2, false);
        self.scratch.slot_put(slot, key, v);
        v
    }
}

/// `shift_a`/`shift_b`/`shift` share one body in vanilla: the noise sampled at
/// a quarter scale and multiplied back up by four.
#[inline]
fn shift(noise: &crate::noise::NormalNoise, x: f64, y: f64, z: f64) -> f64 {
    noise.get_value(x * 0.25, y * 0.25, z * 0.25) * 4.0
}
