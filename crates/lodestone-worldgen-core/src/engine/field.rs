//! The block-field evaluator: reference noise-chunk semantics over a flattened
//! [`Graph`].
//!
//! This is a **recursive descent over indices**, not a bottom-up sweep over the
//! `Vec<Op>`, and that is forced rather than stylistic — see
//! [`Field::eval`]'s `Mul` arm. Three other node kinds also branch
//! (`range_choice`, `interval_select`, and `interpolated`'s two regimes), so the
//! set of nodes an evaluation touches is position-dependent and cannot be known
//! before walking.
//!
//! # Semantics preserved
//!
//! 1. **`Mul`'s `v1 == 0.0` short-circuit** — the second operand is not
//!    evaluated at all.
//! 2. **`interpolated`-inside-corner transparency** — while filling a corner,
//!    a nested `interpolated` is transparent.
//! 3. **`flat_cache`'s quart snap and forced `y = 0`**.
//! 4. **`cache_2d` / `cache_once` scoping** — transparent *here*; `cache_2d` is
//!    a real memo only in the point interpreter.
//! 5. **`cache_all_in_cell`** — transparent as a value, but it selects the
//!    math-helper trilinear lerp (X-inner) over the incremental chain (Y-inner).
//!
//! # Float order
//!
//! Every operation here is IEEE-exact: `+`, `-`, `*`, `/`, `min`, `max`, `abs`,
//! `clamp`, and the math-helper lerp family built from them. No `mul_add`, FMA,
//! reassociation, or transcendental operation is introduced by this walk.

use std::simd::prelude::*;

use super::graph::{Graph, NodeId, Op, OpKind, OverworldFinalDensityPlan};
use super::scratch::Scratch;
use crate::density::Context;

/// Cell geometry selected from the owning settings document. The usual values
/// are 4 and 8; the End uses 8 and 4.
#[derive(Clone, Copy, Debug)]
pub struct Geom {
    /// Cell width along X/Z.
    pub cell_width: i32,
    /// Cell height along Y.
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
    ops: &'a [Op],
    params: &'a [f64],
    children: &'a [NodeId],
    noises: &'a [crate::noise::NormalNoise],
    leaves: &'a [crate::density::Density],
    geom: Geom,
    scratch: &'a mut Scratch,
    column_xz: Option<ColumnXZ>,
    cell_xz: Option<(usize, usize)>,
    column_y: Option<(i32, i32, f64)>,
}

#[derive(Clone, Copy)]
struct ColumnXZ {
    x: i32,
    z: i32,
    cx: i32,
    cz: i32,
    fx: f64,
    fz: f64,
}

/// The four corner channels needed by the bounded final-density noodle shape.
/// Keeping corners, rather than 128 interpolated values, makes the fused path
/// fixed-size while retaining the ordinary channel evaluation order.
#[derive(Clone, Copy)]
struct NoodleCorners {
    channels: [[f64; 8]; 4],
}

impl Default for NoodleCorners {
    fn default() -> Self {
        Self {
            channels: [[0.0; 8]; 4],
        }
    }
}

impl<'a> Field<'a> {
    pub(crate) fn new(graph: &'a Graph, geom: Geom, scratch: &'a mut Scratch) -> Self {
        Self {
            graph,
            ops: graph.ops(),
            params: graph.params(),
            children: graph.children(),
            noises: graph.noises(),
            leaves: graph.leaves(),
            geom,
            scratch,
            column_xz: None,
            cell_xz: None,
            column_y: None,
        }
    }

    pub(crate) fn eval_column(
        &mut self,
        id: NodeId,
        x: i32,
        z: i32,
        y_start: i32,
        output: &mut [f64],
    ) {
        let cw = self.geom.cell_width;
        self.column_xz = Some(ColumnXZ {
            x,
            z,
            cx: x.div_euclid(cw),
            cz: z.div_euclid(cw),
            fx: f64::from(x.rem_euclid(cw)) / f64::from(cw),
            fz: f64::from(z.rem_euclid(cw)) / f64::from(cw),
        });
        self.cell_xz = self
            .scratch
            .cell_column_indices(x.div_euclid(cw), z.div_euclid(cw));
        self.scratch.begin_column();
        let ch = self.geom.cell_height;
        for (offset, value) in output.iter_mut().enumerate() {
            let y = y_start + offset as i32;
            self.column_y = Some((
                y,
                y.div_euclid(ch),
                f64::from(y.rem_euclid(ch)) / f64::from(ch),
            ));
            *value = self.eval(id, x, y, z, true);
        }
        self.column_xz = None;
        self.cell_xz = None;
        self.column_y = None;
    }

    pub(crate) fn eval_overworld_final_density_cell(
        &mut self,
        plan: OverworldFinalDensityPlan,
        x0: i32,
        y0: i32,
        z0: i32,
        output: &mut [f64; 128],
    ) {
        debug_assert_eq!(self.geom.cell_width, 4);
        debug_assert_eq!(self.geom.cell_height, 8);
        debug_assert_eq!(x0.rem_euclid(self.geom.cell_width), 0);
        debug_assert_eq!(y0.rem_euclid(self.geom.cell_height), 0);
        debug_assert_eq!(z0.rem_euclid(self.geom.cell_width), 0);
        self.column_xz = None;
        self.column_y = None;
        self.cell_xz = self
            .scratch
            .cell_column_indices(
                x0.div_euclid(self.geom.cell_width),
                z0.div_euclid(self.geom.cell_width),
            );

        let all = [u64::MAX; 2];
        self.interpolate_cell(
            plan.terrain_inner,
            plan.terrain_slot,
            x0,
            y0,
            z0,
            all,
            output,
        );

        if self
            .interpolate_shared_noodle_cell(plan, x0, y0, z0, output)
            .is_some()
        {
            self.cell_xz = None;
            return;
        }

        {
            let mut control = [0.0; 128];
            let mut ridge = [0.0; 128];
            let mut values = [0.0; 128];
            self.interpolate_cell(
                plan.noodle_control_inner,
                plan.noodle_control_slot,
                x0,
                y0,
                z0,
                all,
                &mut control,
            );

            let mut mask = [0_u64; 2];
            for (index, value) in control.iter().copied().enumerate() {
                if !(value >= -1_000_000.0 && value < 0.0) {
                    mask[index >> 6] |= 1_u64 << (index & 63);
                }
            }

            if mask != [0; 2] {
                self.interpolate_cell(
                    plan.noodle_ridge_a_inner,
                    plan.noodle_ridge_a_slot,
                    x0,
                    y0,
                    z0,
                    mask,
                    &mut values,
                );
                for (index, value) in values.iter().copied().enumerate() {
                    if mask_contains(mask, index) {
                        ridge[index] = value.abs();
                    }
                }
                self.interpolate_cell(
                    plan.noodle_ridge_b_inner,
                    plan.noodle_ridge_b_slot,
                    x0,
                    y0,
                    z0,
                    mask,
                    &mut values,
                );
                for (index, value) in values.iter().copied().enumerate() {
                    if mask_contains(mask, index) {
                        ridge[index] = ridge[index].max(value.abs());
                    }
                }
                self.interpolate_cell(
                    plan.noodle_thickness_inner,
                    plan.noodle_thickness_slot,
                    x0,
                    y0,
                    z0,
                    mask,
                    &mut values,
                );
            }
            let out_mask = mask;

            for (index, terrain) in output.iter_mut().enumerate() {
                let squeezed = (*terrain).clamp(-1.0, 1.0);
                let squeezed = squeezed / 2.0 - squeezed * squeezed * squeezed / 24.0;
                let noodle = if mask_contains(out_mask, index) {
                    values[index] + 1.5 * ridge[index]
                } else {
                    64.0
                };
                *terrain = squeezed.min(noodle);
            }
        }
        self.cell_xz = None;
    }

    /// Evaluates `id` at a block through the field wrapper.
    pub(crate) fn eval(&mut self, id: NodeId, x: i32, y: i32, z: i32, interpolate: bool) -> f64 {
        let op = self.ops[id as usize];
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
            OpKind::Const => self.params[op.a as usize],
            OpKind::BlendAlpha => 1.0,
            OpKind::BlendOffset | OpKind::Beardifier => 0.0,
            OpKind::YClampedGradient => {
                let p = op.a;
                crate::math::clamped_map(
                    f64::from(y),
                    self.params[p as usize],
                    self.params[(p + 1) as usize],
                    self.params[(p + 2) as usize],
                    self.params[(p + 3) as usize],
                )
            }

            OpKind::Add => {
                self.eval(op.a, x, y, z, interpolate) + self.eval(op.b, x, y, z, interpolate)
            }
            // Semantic 1. The reference evaluator does not evaluate the second operand when the
            // first is exactly zero. This is why the evaluator is a recursive
            // descent: a bottom-up sweep over `ops` would evaluate every node,
            // which is not merely slower — a skipped subtree can contain a
            // `flat_cache`/`interpolated` slot write, so evaluating it would
            // populate caches the reference evaluator leaves empty and change what a later
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
                v.clamp(self.params[op.b as usize], self.params[(op.b + 1) as usize])
            }

            OpKind::Interpolated => {
                if interpolate {
                    let slot = op.b as usize;
                    if self.column_xz.is_some() {
                        let ch = self.geom.cell_height;
                        let cy = y.div_euclid(ch);
                        let offset = y.rem_euclid(ch) as usize;
                        if let Some(value) = self.scratch.column_value(slot, cy, offset) {
                            value
                        } else {
                            self.interpolate_cell_column(op.a, slot, x, z, cy);
                            self.scratch
                                .column_value(slot, cy, offset)
                                .expect("cell-column interpolation was populated")
                        }
                    } else {
                        self.interpolate(op.a, slot, x, y, z)
                    }
                } else {
                    // Semantic 2. Nested inside a corner sample: the interpolator
                    // interpolator is transparent when the context is not the
                    // field wrapper itself. The flag has to thread through the
                    // descent, which is the only reason `eval` carries it.
                    self.eval(op.a, x, y, z, false)
                }
            }
            // Semantic 3. XZ snapped to the quart grid (multiples of 4,
            // hardcoded to four blocks — deliberately *not* `cell_width`) and `y`
            // forced to exactly 0, so a 2D climate/shift field is sampled once
            // per 4x4 column. The inner is evaluated with `interpolate = false`.
            OpKind::FlatCache => {
                let qx = (x >> 2) << 2;
                let qz = (z >> 2) << 2;
                self.slot_get(op.b as usize, (qx, 0, qz), op.a)
            }
            // Memoised on the last `(x, y, z)` — see `Scratch::leaf_get`. Safe
            // here and at the leaf arm below, and at no other arm, because these
            // are the only kinds with no field children: nothing under them can
            // write a cache slot a later query depends on.
            OpKind::Noise => {
                if let Some(v) = self.scratch.leaf_get(id, x, y, z) {
                    return v;
                }
                crate::counters::bump_cache_compute(crate::counters::CacheKind::Leaf);
                let n = &self.noises[op.a as usize];
                let xz = self.params[op.b as usize];
                let ys = self.params[(op.b + 1) as usize];
                let v = n.get_value(f64::from(x) * xz, f64::from(y) * ys, f64::from(z) * xz);
                self.scratch.leaf_put(id, x, y, z, v);
                v
            }
            OpKind::ShiftedNoise => {
                let (c0, c1, c2) = (
                    self.children[op.a as usize],
                    self.children[(op.a + 1) as usize],
                    self.children[(op.a + 2) as usize],
                );
                let xz = self.params[op.c as usize];
                let ys = self.params[(op.c + 1) as usize];
                let sx = f64::from(x) * xz + self.eval(c0, x, y, z, interpolate);
                let sy = f64::from(y) * ys + self.eval(c1, x, y, z, interpolate);
                let sz = f64::from(z) * xz + self.eval(c2, x, y, z, interpolate);
                self.noises[op.b as usize].get_value(sx, sy, sz)
            }
            OpKind::ShiftA => {
                shift(&self.noises[op.a as usize], f64::from(x), 0.0, f64::from(z))
            }
            OpKind::ShiftB => {
                shift(&self.noises[op.a as usize], f64::from(z), f64::from(x), 0.0)
            }
            OpKind::Shift => shift(
                &self.noises[op.a as usize],
                f64::from(x),
                f64::from(y),
                f64::from(z),
            ),

            OpKind::RangeChoice => {
                let input = self.children[op.a as usize];
                let v = self.eval(input, x, y, z, interpolate);
                let lo = self.params[op.b as usize];
                let hi = self.params[(op.b + 1) as usize];
                let branch = if v >= lo && v < hi {
                    self.children[(op.a + 1) as usize]
                } else {
                    self.children[(op.a + 2) as usize]
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
                let n = self.children[op.a as usize];
                let input = self.children[(op.a + 1) as usize];
                let v = self.eval(input, x, y, z, interpolate);
                for i in 0..op.c {
                    if v < self.params[(op.b + i) as usize] {
                        let branch = self.children[(op.a + 2 + i) as usize];
                        return self.eval(branch, x, y, z, interpolate);
                    }
                }
                let branch = self.children[(op.a + 1 + n) as usize];
                self.eval(branch, x, y, z, interpolate)
            }

            // The three point-evaluated leaves. The field evaluator does not
            // recurse into these; it calls the point interpreter, so everything
            // beneath one of them has point semantics (no quart snapping, no
            // interpolation). This is an observable semantic — see
            // `super::graph`'s module doc — and it is why these hold an
            // untouched `Density` subtree rather than compiled nodes.
            //
            // Memoised on the last `(x, y, z)` because these leaves are reached
            // repeatedly while filling a cell.
            OpKind::Spline | OpKind::Blended | OpKind::FindTopSurface | OpKind::EndIslands => {
                if let Some(v) = self.scratch.leaf_get(id, x, y, z) {
                    return v;
                }
                crate::counters::bump_cache_compute(crate::counters::CacheKind::Leaf);
                let v = self.leaves[op.a as usize].compute(Context::new(x, y, z));
                self.scratch.leaf_put(id, x, y, z, v);
                v
            }
        }
    }

    fn interpolate_shared_noodle_cell(
        &mut self,
        plan: OverworldFinalDensityPlan,
        x0: i32,
        y0: i32,
        z0: i32,
        output: &mut [f64; 128],
    ) -> Option<()> {
        // The common input is evaluated once, while each range branch and cache
        // write keeps the ordinary control/ridge/thickness order.
        let (selector, roots) = self.shared_noodle_ranges(plan)?;
        let cw = self.geom.cell_width;
        let ch = self.geom.cell_height;
        let cx = x0.div_euclid(cw);
        let cy = y0.div_euclid(ch);
        let cz = z0.div_euclid(cw);

        if roots.iter().any(|&(_, slot)| {
            self.scratch
                .cell_is_present(slot, cx, cy, cz, self.cell_xz)
        }) {
            return None;
        }

        let mut carrier = NoodleCorners::default();
        let mut selector_values = [0.0; 8];
        let mut selector_known = [false; 8];
        carrier.channels[0] = self.cell_corners_shared_range(
            roots[0].0,
            roots[0].1,
            selector,
            cx,
            cy,
            cz,
            cw,
            ch,
            &mut selector_values,
            &mut selector_known,
        );
        let mut out_mask = [0_u64; 2];
        for lz in 0..cw {
            for lx in 0..cw {
                for ly in 0..ch {
                    let index = ((lz * cw + lx) * ch + ly) as usize;
                    let value = interpolate_lane(cw, ch, index, &carrier.channels[0]);
                    if !(value >= -1_000_000.0 && value < 0.0) {
                        out_mask[index >> 6] |= 1_u64 << (index & 63);
                    }
                }
            }
        }
        if out_mask == [0; 2] {
            write_shared_noodle_output(cw, ch, out_mask, &carrier.channels, output);
            return Some(());
        }

        carrier.channels[1] = self.cell_corners_shared_range(
            roots[1].0,
            roots[1].1,
            selector,
            cx,
            cy,
            cz,
            cw,
            ch,
            &mut selector_values,
            &mut selector_known,
        );
        carrier.channels[2] = self.cell_corners_shared_range(
            roots[2].0,
            roots[2].1,
            selector,
            cx,
            cy,
            cz,
            cw,
            ch,
            &mut selector_values,
            &mut selector_known,
        );
        carrier.channels[3] = self.cell_corners_shared_range(
            roots[3].0,
            roots[3].1,
            selector,
            cx,
            cy,
            cz,
            cw,
            ch,
            &mut selector_values,
            &mut selector_known,
        );
        write_shared_noodle_output(cw, ch, out_mask, &carrier.channels, output);
        Some(())
    }

    fn shared_noodle_ranges(
        &self,
        plan: OverworldFinalDensityPlan,
    ) -> Option<(NodeId, [(NodeId, usize); 4])> {
        let roots = [
            plan.noodle_control_inner,
            plan.noodle_ridge_a_inner,
            plan.noodle_ridge_b_inner,
            plan.noodle_thickness_inner,
        ];
        let first = self.ops[*roots.first()? as usize];
        if first.kind != OpKind::RangeChoice {
            return None;
        }
        let selector = self.children[first.a as usize];
        let selector_op = self.ops[selector as usize];
        if selector_op.kind != OpKind::YClampedGradient {
            return None;
        }
        for &root in &roots {
            let op = self.ops[root as usize];
            if op.kind != OpKind::RangeChoice
                || op.b != first.b
                || op.c != first.c
                || self.children[op.a as usize] != selector
                || self.has_slot_writer(self.children[(op.a + 1) as usize])
                || self.has_slot_writer(self.children[(op.a + 2) as usize])
            {
                return None;
            }
        }
        Some((
            selector,
            [
                (roots[0], plan.noodle_control_slot),
                (roots[1], plan.noodle_ridge_a_slot),
                (roots[2], plan.noodle_ridge_b_slot),
                (roots[3], plan.noodle_thickness_slot),
            ],
        ))
    }

    fn has_slot_writer(&self, id: NodeId) -> bool {
        let op = self.ops[id as usize];
        match op.kind {
            OpKind::Interpolated | OpKind::FlatCache => true,
            OpKind::Const
            | OpKind::BlendAlpha
            | OpKind::BlendOffset
            | OpKind::Beardifier
            | OpKind::YClampedGradient
            | OpKind::Noise
            | OpKind::ShiftA
            | OpKind::ShiftB
            | OpKind::Shift
            | OpKind::Spline
            | OpKind::Blended
            | OpKind::FindTopSurface
            | OpKind::EndIslands => false,
            OpKind::Add
            | OpKind::Mul
            | OpKind::Min
            | OpKind::Max
            | OpKind::Abs
            | OpKind::Square
            | OpKind::Cube
            | OpKind::HalfNegative
            | OpKind::QuarterNegative
            | OpKind::Squeeze
            | OpKind::Invert
            | OpKind::Clamp => self.has_slot_writer(op.a),
            OpKind::ShiftedNoise => {
                self.has_slot_writer(self.children[op.a as usize])
                    || self.has_slot_writer(self.children[(op.a + 1) as usize])
                    || self.has_slot_writer(self.children[(op.a + 2) as usize])
            }
            OpKind::RangeChoice => {
                self.has_slot_writer(self.children[op.a as usize])
                    || self.has_slot_writer(self.children[(op.a + 1) as usize])
                    || self.has_slot_writer(self.children[(op.a + 2) as usize])
            }
            OpKind::IntervalSelect => {
                let n = self.children[op.a as usize] as usize;
                self.has_slot_writer(self.children[(op.a + 1) as usize])
                    || (0..n).any(|i| {
                        self.has_slot_writer(self.children[(op.a + 2 + i as u32) as usize])
                    })
            }
        }
    }


    fn cell_corners_shared_range(
        &mut self,
        range: NodeId,
        slot: usize,
        selector: NodeId,
        cx: i32,
        cy: i32,
        cz: i32,
        cw: i32,
        ch: i32,
        selector_values: &mut [f64; 8],
        selector_known: &mut [bool; 8],
    ) -> [f64; 8] {
        crate::counters::bump_cell_fill();
        crate::counters::bump_cache_compute(crate::counters::CacheKind::Cell);
        let (x0, y0, z0) = (cx * cw, cy * ch, cz * cw);
        let (x1, y1, z1) = (x0 + cw, y0 + ch, z0 + cw);
        let keys = [
            (x0, y0, z0),
            (x1, y0, z0),
            (x0, y1, z0),
            (x1, y1, z0),
            (x0, y0, z1),
            (x1, y0, z1),
            (x0, y1, z1),
            (x1, y1, z1),
        ];
        let mut values = [0.0; 8];
        for (index, key) in keys.into_iter().enumerate() {
            crate::counters::bump_corner_lookup();
            if let Some(value) = self.scratch.slot_get(slot, key) {
                crate::counters::bump_slot_hit();
                values[index] = value;
                continue;
            }
            crate::counters::bump_slot_miss(slot);
            crate::counters::bump_corner_eval();
            crate::counters::bump_cache_compute(crate::counters::CacheKind::Slot);
            if !selector_known[index] {
                selector_values[index] = self.eval(selector, key.0, key.1, key.2, false);
                selector_known[index] = true;
            }
            let value = self.eval_range_branch(range, selector_values[index], key);
            self.scratch.slot_put(slot, key, value);
            values[index] = value;
        }
        self.scratch.cell_put(slot, cx, cy, cz, values);
        values
    }

    fn eval_range_branch(
        &mut self,
        id: NodeId,
        input: f64,
        (x, y, z): (i32, i32, i32),
    ) -> f64 {
        let op = self.ops[id as usize];
        debug_assert_eq!(op.kind, OpKind::RangeChoice);
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
        let branch = if input >= self.params[op.b as usize]
            && input < self.params[(op.b + 1) as usize]
        {
            self.children[(op.a + 1) as usize]
        } else {
            self.children[(op.a + 2) as usize]
        };
        self.eval(branch, x, y, z, false)
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
    /// This cache-all-in-cell hoist holds the eight corners
    /// rather than the 128 interpolated values, which costs the same arithmetic
    /// and a 128th of the memory.
    ///
    /// # Semantic 5: which interpolation order
    ///
    /// The trilinear lerp order is X-inner, then Y, then Z. The incremental
    /// Y-inner chain is numerically different, so this nesting is part of the
    /// exact output contract.
    fn interpolate(&mut self, inner: NodeId, slot: usize, x: i32, y: i32, z: i32) -> f64 {
        let cw = self.geom.cell_width;
        let ch = self.geom.cell_height;
        let (cx, cz, fx, fz) = if let Some(column) = self.column_xz {
            debug_assert_eq!((x, z), (column.x, column.z));
            (column.cx, column.cz, column.fx, column.fz)
        } else {
            (
                x.div_euclid(cw),
                z.div_euclid(cw),
                f64::from(x.rem_euclid(cw)) / f64::from(cw),
                f64::from(z.rem_euclid(cw)) / f64::from(cw),
            )
        };
        let (cy, fy) = if let Some((column_y, cy, fy)) = self.column_y {
            debug_assert_eq!(y, column_y);
            (cy, fy)
        } else {
            (y.div_euclid(ch), f64::from(y.rem_euclid(ch)) / f64::from(ch))
        };

        let n = self.cell_corners(inner, slot, cx, cy, cz, cw, ch);

        crate::math::lerp3(fx, fy, fz, n[0], n[1], n[2], n[3], n[4], n[5], n[6], n[7])
    }

    fn interpolate_cell_column(
        &mut self,
        inner: NodeId,
        slot: usize,
        x: i32,
        z: i32,
        cy: i32,
    ) {
        let column = self.column_xz.expect("column interpolation needs fixed x/z");
        debug_assert_eq!((x, z), (column.x, column.z));
        let cw = self.geom.cell_width;
        let ch = self.geom.cell_height;
        let mut values = self.scratch.take_column_values(slot);
        values.resize(ch as usize, 0.0);
        let n = self.cell_corners(inner, slot, column.cx, cy, column.cz, cw, ch);
        let x00 = crate::math::lerp(column.fx, n[0], n[1]);
        let x10 = crate::math::lerp(column.fx, n[2], n[3]);
        let x01 = crate::math::lerp(column.fx, n[4], n[5]);
        let x11 = crate::math::lerp(column.fx, n[6], n[7]);
        for (offset, value) in values.iter_mut().enumerate() {
            let fy = offset as f64 / f64::from(ch);
            *value = crate::math::lerp(
                column.fz,
                crate::math::lerp(fy, x00, x10),
                crate::math::lerp(fy, x01, x11),
            );
        }
        self.scratch.put_column_values(slot, cy, values);
    }

    fn interpolate_cell(
        &mut self,
        inner: NodeId,
        slot: usize,
        x0: i32,
        y0: i32,
        z0: i32,
        mask: [u64; 2],
        output: &mut [f64; 128],
    ) {
        let cw = self.geom.cell_width;
        let ch = self.geom.cell_height;
        let cx = x0.div_euclid(cw);
        let cy = y0.div_euclid(ch);
        let cz = z0.div_euclid(cw);
        let n = self.cell_corners(inner, slot, cx, cy, cz, cw, ch);
        write_interpolated_corners(cw, ch, x0, y0, z0, mask, n, output);
    }

    fn cell_corners(
        &mut self,
        inner: NodeId,
        slot: usize,
        cx: i32,
        cy: i32,
        cz: i32,
        cw: i32,
        ch: i32,
    ) -> [f64; 8] {
        if let Some(n) = self
            .scratch
            .cell_get_ref_with_column(slot, cx, cy, cz, self.cell_xz)
        {
            return *n;
        }

        crate::counters::bump_cell_fill();
        crate::counters::bump_cache_compute(crate::counters::CacheKind::Cell);
        let (x0, y0, z0) = (cx * cw, cy * ch, cz * cw);
        let (x1, y1, z1) = (x0 + cw, y0 + ch, z0 + cw);
        let n = [
            self.corner(inner, slot, x0, y0, z0),
            self.corner(inner, slot, x1, y0, z0),
            self.corner(inner, slot, x0, y1, z0),
            self.corner(inner, slot, x1, y1, z0),
            self.corner(inner, slot, x0, y0, z1),
            self.corner(inner, slot, x1, y0, z1),
            self.corner(inner, slot, x0, y1, z1),
            self.corner(inner, slot, x1, y1, z1),
        ];
        self.scratch.cell_put(slot, cx, cy, cz, n);
        n
    }

    /// Fetch one cell corner and keep corner counters separate from slot-cache
    /// counters. The memo lookup stays inline on this hot path.
    fn corner(&mut self, inner: NodeId, slot: usize, x: i32, y: i32, z: i32) -> f64 {
        crate::counters::bump_corner_lookup();
        let key = (x, y, z);
        if let Some(v) = self.scratch.slot_get(slot, key) {
            crate::counters::bump_slot_hit();
            return v;
        }
        crate::counters::bump_slot_miss(slot);
        crate::counters::bump_corner_eval();
        crate::counters::bump_cache_compute(crate::counters::CacheKind::Slot);
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
        crate::counters::bump_cache_compute(crate::counters::CacheKind::Slot);
        let v = self.eval(inner, key.0, key.1, key.2, false);
        self.scratch.slot_put(slot, key, v);
        v
    }
}

fn write_interpolated_corners(
    cw: i32,
    ch: i32,
    _x0: i32,
    _y0: i32,
    _z0: i32,
    mask: [u64; 2],
    n: [f64; 8],
    output: &mut [f64; 128],
) {
    if cw == 4 && ch == 8 {
        write_interpolated_corners_simd(mask, n, output);
    } else {
        write_interpolated_corners_scalar(cw, ch, mask, n, output);
    }
}

fn write_interpolated_corners_scalar(
    cw: i32,
    ch: i32,
    mask: [u64; 2],
    n: [f64; 8],
    output: &mut [f64; 128],
) {
    for lz in 0..cw {
        for lx in 0..cw {
            for ly in 0..ch {
                let index = ((lz * cw + lx) * ch + ly) as usize;
                if mask_contains(mask, index) {
                    *output.get_mut(index).expect("4x8x4 cell index") = crate::math::lerp3(
                        f64::from(lx) / f64::from(cw),
                        f64::from(ly) / f64::from(ch),
                        f64::from(lz) / f64::from(cw),
                        n[0],
                        n[1],
                        n[2],
                        n[3],
                        n[4],
                        n[5],
                        n[6],
                        n[7],
                    );
                }
            }
        }
    }
}

fn write_interpolated_corners_simd(
    mask: [u64; 2],
    n: [f64; 8],
    output: &mut [f64; 128],
) {
    for index in (0..128).step_by(8) {
        let values = interpolate_canonical_chunk(index, &n).to_array();
        for lane in 0..8 {
            if mask_contains(mask, index + lane) {
                output[index + lane] = values[lane];
            }
        }
    }
}

fn write_shared_noodle_output(
    cw: i32,
    ch: i32,
    mask: [u64; 2],
    channels: &[[f64; 8]; 4],
    output: &mut [f64; 128],
) {
    if cw == 4 && ch == 8 {
        write_shared_noodle_output_simd(mask, channels, output);
    } else {
        write_shared_noodle_output_scalar(cw, ch, mask, channels, output);
    }
}

fn write_shared_noodle_output_scalar(
    cw: i32,
    ch: i32,
    mask: [u64; 2],
    channels: &[[f64; 8]; 4],
    output: &mut [f64; 128],
) {
    for (index, terrain) in output.iter_mut().enumerate() {
        let squeezed = (*terrain).clamp(-1.0, 1.0);
        let squeezed = squeezed / 2.0 - squeezed * squeezed * squeezed / 24.0;
        let noodle = if mask_contains(mask, index) {
            let ridge_a = interpolate_lane(cw, ch, index, &channels[1]);
            let ridge_b = interpolate_lane(cw, ch, index, &channels[2]);
            let thickness = interpolate_lane(cw, ch, index, &channels[3]);
            thickness + 1.5 * ridge_a.abs().max(ridge_b.abs())
        } else {
            64.0
        };
        *terrain = squeezed.min(noodle);
    }
}

fn write_shared_noodle_output_simd(
    mask: [u64; 2],
    channels: &[[f64; 8]; 4],
    output: &mut [f64; 128],
) {
    for index in (0..128).step_by(8) {
        let mut clamped = [0.0; 8];
        for lane in 0..8 {
            clamped[lane] = output[index + lane].clamp(-1.0, 1.0);
        }
        let clamped = Simd::<f64, 8>::from_array(clamped);
        let squared = clamped * clamped;
        let cubed = squared * clamped;
        let squeezed = clamped / Simd::splat(2.0) - cubed / Simd::splat(24.0);
        let squeezed = squeezed.to_array();

        let ridge_a = interpolate_canonical_chunk(index, &channels[1]).to_array();
        let ridge_b = interpolate_canonical_chunk(index, &channels[2]).to_array();
        let mut active = [false; 8];
        let mut max_abs = [0.0; 8];
        for lane in 0..8 {
            active[lane] = mask_contains(mask, index + lane);
            if active[lane] {
                max_abs[lane] = ridge_a[lane].abs().max(ridge_b[lane].abs());
            }
        }
        let thickness = interpolate_canonical_chunk(index, &channels[3]);
        let noodle = (thickness + Simd::splat(1.5) * Simd::from_array(max_abs)).to_array();
        for lane in 0..8 {
            if active[lane] {
                output[index + lane] = squeezed[lane].min(noodle[lane]);
            } else {
                output[index + lane] = squeezed[lane].min(64.0);
            }
        }
    }
}

#[inline]
fn interpolate_canonical_chunk(start: usize, n: &[f64; 8]) -> Simd<f64, 8> {
    const XZ_FACTORS: [f64; 4] = [0.0, 0.25, 0.5, 0.75];
    const Y_FACTORS: [f64; 8] = [0.0, 0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875];
    debug_assert!(start < 128 && start % 8 == 0);
    let group = start / 8;
    let x = Simd::splat(XZ_FACTORS[group % 4]);
    let y = Simd::from_array(Y_FACTORS);
    let z = Simd::splat(XZ_FACTORS[group / 4]);
    let p0 = Simd::splat(n[0]) + x * (Simd::splat(n[1]) - Simd::splat(n[0]));
    let p1 = Simd::splat(n[2]) + x * (Simd::splat(n[3]) - Simd::splat(n[2]));
    let p2 = Simd::splat(n[4]) + x * (Simd::splat(n[5]) - Simd::splat(n[4]));
    let p3 = Simd::splat(n[6]) + x * (Simd::splat(n[7]) - Simd::splat(n[6]));
    let q0 = p0 + y * (p1 - p0);
    let q1 = p2 + y * (p3 - p2);
    q0 + z * (q1 - q0)
}

#[inline]
fn interpolate_lane(cw: i32, ch: i32, index: usize, n: &[f64; 8]) -> f64 {
    let plane = (cw * ch) as usize;
    let lz = index / plane;
    let within = index % plane;
    let lx = within / ch as usize;
    let ly = within % ch as usize;
    crate::math::lerp3(
        lx as f64 / f64::from(cw),
        ly as f64 / f64::from(ch),
        lz as f64 / f64::from(cw),
        n[0],
        n[1],
        n[2],
        n[3],
        n[4],
        n[5],
        n[6],
        n[7],
    )
}

#[inline]
fn mask_contains(mask: [u64; 2], index: usize) -> bool {
    (mask[index >> 6] & (1_u64 << (index & 63))) != 0
}

/// `shift_a`/`shift_b`/`shift` share one body: the noise sampled at
/// a quarter scale and multiplied back up by four.
#[inline]
fn shift(noise: &crate::noise::NormalNoise, x: f64, y: f64, z: f64) -> f64 {
    noise.get_value(x * 0.25, y * 0.25, z * 0.25) * 4.0
}

#[cfg(test)]
mod tests {
    use super::{
        write_interpolated_corners, write_interpolated_corners_scalar, write_shared_noodle_output,
        write_shared_noodle_output_scalar,
    };
    #[cfg(feature = "gen-counters")]
    use crate::counters;
    use crate::density::Density;
    use crate::engine::{Bounds, Program};

    #[test]
    fn interpolated_corner_simd_matches_scalar_for_all_lanes_and_masks() {
        let corner_sets = [
            [-3.25, 0.125, 7.5, -11.75, 19.0, -0.0, 2.5, -4.0],
            [f64::NAN, 0.125, 7.5, -11.75, 19.0, -0.0, 2.5, -4.0],
        ];
        let masks = [
            [0, 0],
            [u64::MAX, u64::MAX],
            [1, 0],
            [0, 1],
            [1_u64 << 63, 1],
            [0xaaaa_aaaa_aaaa_aaaa, 0x5555_5555_5555_5555],
        ];
        for &(x0, y0, z0) in &[(-8, -16, -8), (0, 0, 0), (4, 8, 4)] {
            for corners in corner_sets {
                for mask in masks {
                    let mut actual = [f64::from_bits(0x8000_0000_0000_0000); 128];
                    let mut expected = actual;
                    write_interpolated_corners(
                        4, 8, x0, y0, z0, mask, corners, &mut actual,
                    );
                    write_interpolated_corners_scalar(4, 8, mask, corners, &mut expected);
                    for (index, (&got, &want)) in actual.iter().zip(expected.iter()).enumerate() {
                        assert_eq!(
                            got.to_bits(),
                            want.to_bits(),
                            "corner lane {index}, origin ({x0},{y0},{z0}), mask {mask:?}",
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn shared_noodle_output_simd_matches_scalar_for_all_lanes_and_masks() {
        let channels = [
            [0.0; 8],
            [-3.25, 0.125, 7.5, -11.75, 19.0, -0.0, 2.5, -4.0],
            [4.5, -2.0, 0.25, 8.0, -9.0, 1.0, -0.5, 3.0],
            [0.75, -1.25, 2.5, -4.0, 5.0, -0.0, 0.125, 6.0],
        ];
        let masks = [
            [0, 0],
            [u64::MAX, u64::MAX],
            [1, 0],
            [0, 1],
            [1_u64 << 63, 1],
            [0xaaaa_aaaa_aaaa_aaaa, 0x5555_5555_5555_5555],
        ];
        for mask in masks {
            let mut actual = [0.0; 128];
            for (index, value) in actual.iter_mut().enumerate() {
                *value = match index % 8 {
                    0 => -2.0,
                    1 => -0.0,
                    2 => 0.0,
                    3 => 0.25,
                    4 => 1.5,
                    5 => f64::NAN,
                    6 => 4.0,
                    _ => -1.25,
                };
            }
            let mut expected = actual;
            write_shared_noodle_output(4, 8, mask, &channels, &mut actual);
            write_shared_noodle_output_scalar(4, 8, mask, &channels, &mut expected);
            for (index, (&got, &want)) in actual.iter().zip(expected.iter()).enumerate() {
                assert_eq!(
                    got.to_bits(),
                    want.to_bits(),
                    "noodle lane {index}, mask {mask:?}",
                );
            }
        }
    }

    #[cfg(feature = "gen-counters")]
    #[test]
    fn shared_noodle_selector_has_an_exact_evaluation_count() {
        let selector = Density::YClampedGradient {
            from_y: 0.0,
            to_y: 8.0,
            from_value: 1.0,
            to_value: 1.0,
        };
        let range = |branch: f64| Density::RangeChoice {
            input: Box::new(selector.clone()),
            min_inclusive: -1_000_000.0,
            max_exclusive: 0.0,
            when_in_range: Box::new(Density::Const(-1.0)),
            when_out_of_range: Box::new(Density::Const(branch)),
        };
        let interpolated = |inner, slot| Density::Interpolated {
            inner: Box::new(inner),
            slot,
        };
        let root = Density::Min(
            Box::new(Density::Squeeze(Box::new(interpolated(
                Density::Const(2.0),
                0,
            )))),
            Box::new(Density::RangeChoice {
                input: Box::new(interpolated(range(1.0), 14)),
                min_inclusive: -1_000_000.0,
                max_exclusive: 0.0,
                when_in_range: Box::new(Density::Const(64.0)),
                when_out_of_range: Box::new(Density::Add(
                    Box::new(interpolated(range(0.5), 15)),
                    Box::new(Density::Mul(
                        Box::new(Density::Const(1.5)),
                        Box::new(Density::Max(
                            Box::new(Density::Abs(Box::new(interpolated(
                                range(0.25),
                                16,
                            )))),
                            Box::new(Density::Abs(Box::new(interpolated(
                                range(0.75),
                                17,
                            )))),
                        )),
                    )),
                )),
            }),
        );
        let program = Program::compile(&root);
        assert!(program.has_overworld_final_density_cell_plan());
        let sampler = crate::density::NoiseChunkSampler::from_program(
            program.clone(),
            18,
            4,
            8,
            Some(Bounds {
                x: (0, 3),
                y: (0, 7),
                z: (0, 3),
            }),
        );

        counters::reset();
        let mut output = [0.0; 128];
        sampler.final_density_cell(0, 0, 0, &mut output);
        let counts = counters::snapshot();
        let scalar = crate::density::NoiseChunkSampler::from_program(
            program,
            18,
            4,
            8,
            Some(Bounds {
                x: (0, 3),
                y: (0, 7),
                z: (0, 3),
            }),
        );
        for lz in 0..4 {
            for lx in 0..4 {
                for ly in 0..8 {
                    let index = ((lz * 4 + lx) * 8 + ly) as usize;
                    assert_eq!(
                        output[index].to_bits(),
                        scalar.final_density(lx, ly, lz).to_bits(),
                        "fused output differs at ({lx},{ly},{lz})"
                    );
                }
            }
        }
        let y_index = Density::KIND_NAMES
            .iter()
            .position(|name| *name == "y_clamped_gradient")
            .expect("y_clamped_gradient kind");
        let range_index = Density::KIND_NAMES
            .iter()
            .position(|name| *name == "range_choice")
            .expect("range_choice kind");
        assert_eq!(counts.density_evals[y_index], 8);
        assert_eq!(counts.density_evals[range_index], 32);
        assert_eq!(counts.corner_evals, 40);
        assert_eq!(counts.cell_fills, 5);
    }

    #[cfg(not(feature = "gen-counters"))]
    #[test]
    fn shared_noodle_cell_is_bitwise_at_negative_boundaries_and_with_inactive_lanes() {
        let build = |selector: Density| {
            let interpolated = |inner, slot| Density::Interpolated {
                inner: Box::new(inner),
                slot,
            };
            let range = |in_range: f64, out_of_range: f64| Density::RangeChoice {
                input: Box::new(selector.clone()),
                min_inclusive: -1_000_000.0,
                max_exclusive: 0.0,
                when_in_range: Box::new(Density::Const(in_range)),
                when_out_of_range: Box::new(Density::Const(out_of_range)),
            };
            Density::Min(
                Box::new(Density::Squeeze(Box::new(interpolated(
                    Density::YClampedGradient {
                        from_y: -16.0,
                        to_y: 16.0,
                        from_value: -2.0,
                        to_value: 2.0,
                    },
                    0,
                )))),
                Box::new(Density::RangeChoice {
                    input: Box::new(interpolated(range(-2.0, 2.0), 14)),
                    min_inclusive: -1_000_000.0,
                    max_exclusive: 0.0,
                    when_in_range: Box::new(Density::Const(64.0)),
                    when_out_of_range: Box::new(Density::Add(
                        Box::new(interpolated(range(0.25, -0.75), 15)),
                        Box::new(Density::Mul(
                            Box::new(Density::Const(1.5)),
                            Box::new(Density::Max(
                                Box::new(Density::Abs(Box::new(interpolated(
                                    range(-1.25, 0.5),
                                    16,
                                )))),
                                Box::new(Density::Abs(Box::new(interpolated(
                                    range(0.75, -1.5),
                                    17,
                                )))),
                            )),
                        )),
                    )),
                }),
            )
        };
        let bounds = Bounds {
            x: (-8, 3),
            y: (-16, 7),
            z: (-8, 3),
        };
        let compare = |selector: Density, origins: &[(i32, i32, i32)]| {
            let program = Program::compile(&build(selector));
            assert!(program.has_overworld_final_density_cell_plan());
            let specialized = crate::density::NoiseChunkSampler::from_program(
                program.clone(),
                18,
                4,
                8,
                Some(bounds),
            );
            let scalar = crate::density::NoiseChunkSampler::from_program(
                program,
                18,
                4,
                8,
                Some(bounds),
            );
            for &(x0, y0, z0) in origins {
                let mut cell = [0.0; 128];
                specialized.final_density_cell(x0, y0, z0, &mut cell);
                for lz in 0..4 {
                    for lx in 0..4 {
                        for ly in 0..8 {
                            let index = ((lz * 4 + lx) * 8 + ly) as usize;
                            let expected = scalar.final_density(x0 + lx, y0 + ly, z0 + lz);
                            assert_eq!(
                                cell[index].to_bits(),
                                expected.to_bits(),
                                "fused output differs at origin ({x0},{y0},{z0}) local ({lx},{ly},{lz})",
                            );
                        }
                    }
                }
            }
        };

        compare(
            Density::YClampedGradient {
                from_y: -8.0,
                to_y: 8.0,
                from_value: -1.0,
                to_value: 1.0,
            },
            &[(-8, -16, -8), (0, -8, 0), (0, 0, 0)],
        );
        compare(
            Density::YClampedGradient {
                from_y: -8.0,
                to_y: 8.0,
                from_value: -1.0,
                to_value: -1.0,
            },
            &[(-8, -16, -8), (0, 0, 0)],
        );
    }
}
