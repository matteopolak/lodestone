//! Recursive block-field evaluation over a flattened [`Graph`].
//!
//! The walk stays recursive because multiplication and selectors can skip
//! subtrees, while interpolation mode is specialized at compile time.

use fearless_simd::{Level, dispatch, f64x8, prelude::*};
use fearless_simd_macros::simd;

use super::graph::{Graph, NodeId, Op, OpKind, OverworldFinalDensityPlan, TilePlan};
use super::scratch::Scratch;
use super::xz_products::XzProductLattice;
use crate::density::Context;
use crate::noise::blended::PreparedBlendedColumn;

pub(crate) struct PreparedBlendedCellColumn {
    node: NodeId,
    x: i32,
    z: i32,
    columns: [Option<PreparedBlendedColumn>; 4],
}

impl PreparedBlendedCellColumn {
    pub(crate) fn new(node: NodeId, x: i32, z: i32) -> Self {
        Self { node, x, z, columns: std::array::from_fn(|_| None) }
    }

    fn compute(&mut self, id: NodeId, noise: &crate::noise::BlendedNoise,
        x: i32, y: i32, z: i32) -> Option<f64>
    {
        if id != self.node { return None; }
        let dx = if x == self.x { 0 } else if x == self.x + 4 { 1 } else { return None; };
        let dz = if z == self.z { 0 } else if z == self.z + 4 { 1 } else { return None; };
        let column = self.columns[dz * 2 + dx].get_or_insert_with(|| noise.prepare_column(x, z));
        Some(noise.compute_prepared(y, column))
    }

    #[cfg(test)]
    pub(crate) fn prepared_count(&self) -> usize {
        self.columns.iter().filter(|column| column.is_some()).count()
    }
}

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
    splines: &'a [super::point::PointProgram],
    geom: Geom,
    scratch: &'a mut Scratch,
    products: Option<&'a XzProductLattice>,
    blended_column: Option<&'a mut PreparedBlendedCellColumn>,
    column_xz: Option<ColumnXZ>,
    cell_xz: Option<(usize, usize)>,
    column_y: Option<(i32, i32, f64)>,
    tile_active: bool,
    #[cfg(feature = "gen-counters")]
    pub(crate) blended_misses: Option<&'a mut (std::collections::BTreeSet<(i32, i32, i32)>, u64)>,
    #[cfg(test)]
    pub(crate) publications: Option<Vec<(usize, (i32, i32, i32), u64)>>,
    #[cfg(test)]
    pub(crate) blended_queries: Option<Vec<(i32, i32, i32)>>,
    #[cfg(any(test, feature = "gen-counters"))]
    pub(crate) deep_saturation_enabled: bool,
    #[cfg(test)]
    pub(crate) force_deep_saturation: bool,
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
    #[cfg(test)]
    pub(crate) fn new(graph: &'a Graph, geom: Geom, scratch: &'a mut Scratch) -> Self {
        Self::new_with_products(graph, geom, scratch, None)
    }

    pub(crate) fn new_with_products(
        graph: &'a Graph,
        geom: Geom,
        scratch: &'a mut Scratch,
        products: Option<&'a XzProductLattice>,
    ) -> Self {
        let products = products.filter(|products| {
            graph
                .product_identity()
                .is_some_and(|identity| products.identity_matches(identity))
        });
        Self {
            graph,
            ops: graph.ops(),
            params: graph.params(),
            children: graph.children(),
            noises: graph.noises(),
            leaves: graph.leaves(),
            splines: graph.splines(),
            geom,
            scratch,
            products,
            blended_column: None,
            column_xz: None,
            cell_xz: None,
            column_y: None,
            tile_active: false,
            #[cfg(feature = "gen-counters")]
            blended_misses: None,
            #[cfg(test)]
            publications: None,
            #[cfg(test)]
            blended_queries: None,
            #[cfg(any(test, feature = "gen-counters"))]
            deep_saturation_enabled: true,
            #[cfg(test)]
            force_deep_saturation: false,
        }
    }

    pub(crate) fn with_blended_column(mut self, column: Option<&'a mut PreparedBlendedCellColumn>) -> Self {
        self.blended_column = column;
        self
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
        let root = self.ops[id as usize];
        if root.kind == OpKind::Interpolated {
            self.scratch.begin_interpolated_column(root.b as usize);
            self.eval_interpolated_column(root, x, z, y_start, output);
        } else if root.kind == OpKind::Squeeze
            && self.ops[root.a as usize].kind == OpKind::Interpolated
        {
            let interpolated = self.ops[root.a as usize];
            self.scratch.begin_interpolated_column(interpolated.b as usize);
            self.eval_interpolated_column(interpolated, x, z, y_start, output);
            squeeze_column(output);
        } else {
            self.scratch.begin_column();
            let ch = self.geom.cell_height;
            for (offset, value) in output.iter_mut().enumerate() {
                let y = y_start + offset as i32;
                self.column_y = Some((
                    y,
                    y.div_euclid(ch),
                    f64::from(y.rem_euclid(ch)) / f64::from(ch),
                ));
                *value = self.eval::<true>(id, x, y, z);
            }
        }
        self.column_xz = None;
        self.cell_xz = None;
        self.column_y = None;
    }

    fn eval_interpolated_column(
        &mut self,
        op: Op,
        x: i32,
        z: i32,
        y_start: i32,
        output: &mut [f64],
    ) {
        let ch = self.geom.cell_height;
        let slot = op.b as usize;
        let mut output_offset = 0;
        while output_offset < output.len() {
            let y = y_start + output_offset as i32;
            let cell_y = y.div_euclid(ch);
            let cell_offset = y.rem_euclid(ch) as usize;
            let len = (ch as usize - cell_offset).min(output.len() - output_offset);
            let range = output_offset..output_offset + len;
            if !self.scratch.copy_column_values(
                slot,
                cell_y,
                cell_offset,
                &mut output[range.clone()],
            ) {
                self.interpolate_cell_column(op.a, slot, x, z, cell_y);
                let copied = self.scratch.copy_column_values(
                    slot,
                    cell_y,
                    cell_offset,
                    &mut output[range],
                );
                debug_assert!(copied, "cell-column interpolation was populated");
            }
            output_offset += len;
        }
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
        self.tile_active = true;
        self.cell_xz = self
            .scratch
            .cell_column_indices(
                x0.div_euclid(self.geom.cell_width),
                z0.div_euclid(self.geom.cell_width),
            );

        if let Some(tile_plan) = self.graph.overworld_final_density_tile_plan() {
            self.eval_overworld_final_density_tile_cell(
                plan, tile_plan, x0, y0, z0, output,
            );
            self.tile_active = false;
            self.cell_xz = None;
            return;
        }

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
            self.tile_active = false;
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
        self.tile_active = false;
        self.cell_xz = None;
    }

    fn eval_overworld_final_density_tile_cell(
        &mut self,
        final_plan: OverworldFinalDensityPlan,
        tile_plan: &TilePlan,
        x0: i32,
        y0: i32,
        z0: i32,
        output: &mut [f64; 128],
    ) {
        debug_assert_eq!(tile_plan.roots.len(), 5);
        let roots = [
            tile_plan.roots[0],
            tile_plan.roots[1],
            tile_plan.roots[2],
            tile_plan.roots[3],
            tile_plan.roots[4],
        ];
        let slots = [
            final_plan.terrain_slot,
            final_plan.noodle_control_slot,
            final_plan.noodle_ridge_a_slot,
            final_plan.noodle_ridge_b_slot,
            final_plan.noodle_thickness_slot,
        ];
        let cw = self.geom.cell_width;
        let ch = self.geom.cell_height;
        let cx = x0.div_euclid(cw);
        let cy = y0.div_euclid(ch);
        let cz = z0.div_euclid(cw);
        let keys = cell_corner_keys(cx, cy, cz, cw, ch);
        let mut corners = [[0.0; 8]; 5];
        let mut active = [0_u8; 5];

        for root in 0..5 {
            if let Some(values) = self
                .scratch
                .cell_get_ref_with_column(slots[root], cx, cy, cz, self.cell_xz)
            {
                corners[root] = *values;
                continue;
            }
            crate::counters::bump_cell_fill();
            crate::counters::bump_cache_compute(crate::counters::CacheKind::Cell);
            for (lane, key) in keys.into_iter().enumerate() {
                crate::counters::bump_corner_lookup();
                if let Some(value) = self.scratch.slot_get(slots[root], key) {
                    crate::counters::bump_slot_hit();
                    corners[root][lane] = value;
                } else {
                    crate::counters::bump_slot_miss(slots[root]);
                    crate::counters::bump_corner_eval();
                    crate::counters::bump_cache_compute(crate::counters::CacheKind::Slot);
                    active[root] |= 1 << lane;
                }
            }
        }

        if active.iter().any(|&mask| mask != 0) {
            let computed = self.eval_tile_plan_roots(tile_plan, roots, keys, active);
            for root in 0..5 {
                for (lane, key) in keys.into_iter().enumerate() {
                    if active[root] & (1 << lane) != 0 {
                        let value = computed[root][lane];
                        corners[root][lane] = value;
                        self.publish_slot(slots[root], key, value);
                    }
                }
                if active[root] != 0 {
                    self.scratch
                        .cell_put(slots[root], cx, cy, cz, corners[root]);
                }
            }
        }

        write_interpolated_corners(cw, ch, x0, y0, z0, [u64::MAX; 2], corners[0], output);
        let mut out_mask = [0_u64; 2];
        for index in 0..128 {
            let value = interpolate_lane(cw, ch, index, &corners[1]);
            if !(value >= -1_000_000.0 && value < 0.0) {
                out_mask[index >> 6] |= 1_u64 << (index & 63);
            }
        }
        let channels = [corners[1], corners[2], corners[3], corners[4]];
        write_shared_noodle_output(cw, ch, out_mask, &channels, output);
    }

    /// Proves that every emitted block in one production cell has positive
    /// final density without materialising the 128 lane values.
    ///
    /// The proof uses the eight vertices of the actually emitted sub-box
    /// (`0..3/4` in X/Z and `0..7/8` in Y), not the enclosing cell's far
    /// corners. Trilinear interpolation is multi-affine, so those vertices
    /// bound every emitted lane. A failed proof leaves the same corner and
    /// slot caches as the ordinary evaluator has populated so the caller can
    /// immediately fall back to [`Self::eval_overworld_final_density_cell`].
    pub(crate) fn eval_overworld_final_density_cell_is_positive(
        &mut self,
        plan: OverworldFinalDensityPlan,
        x0: i32,
        y0: i32,
        z0: i32,
    ) -> bool {
        debug_assert_eq!(self.geom.cell_width, 4);
        debug_assert_eq!(self.geom.cell_height, 8);
        debug_assert_eq!(x0.rem_euclid(self.geom.cell_width), 0);
        debug_assert_eq!(y0.rem_euclid(self.geom.cell_height), 0);
        debug_assert_eq!(z0.rem_euclid(self.geom.cell_width), 0);
        self.column_xz = None;
        self.column_y = None;
        self.tile_active = true;
        self.cell_xz = self
            .scratch
            .cell_column_indices(
                x0.div_euclid(self.geom.cell_width),
                z0.div_euclid(self.geom.cell_width),
            );

        let cx = x0.div_euclid(self.geom.cell_width);
        let cy = y0.div_euclid(self.geom.cell_height);
        let cz = z0.div_euclid(self.geom.cell_width);
        let terrain = self.cell_corners(
            plan.terrain_inner,
            plan.terrain_slot,
            cx,
            cy,
            cz,
            self.geom.cell_width,
            self.geom.cell_height,
        );
        let result = terrain_subcell_is_positive(
            self.geom.cell_width,
            self.geom.cell_height,
            terrain,
        ) && self.noodle_subcell_is_positive(plan, cx, cy, cz);
        self.cell_xz = None;
        self.tile_active = false;
        result
    }

    pub(crate) fn pre_corner_terrain_is_negative(
        &mut self,
        plan: OverworldFinalDensityPlan,
        x0: i32,
        y0: i32,
        z0: i32,
    ) -> bool {
        let Some(slope) = plan.pre_corner_slope else { return false; };
        if self.geom.cell_width != 4 || self.geom.cell_height != 8
            || x0.rem_euclid(4) != 0 || y0.rem_euclid(8) != 0 || z0.rem_euclid(4) != 0
            || !(-40..=248).contains(&y0)
        { return false; }
        let (Some(x1), Some(z1)) = (x0.checked_add(4), z0.checked_add(4)) else { return false; };
        self.column_xz = None;
        self.column_y = None;
        let keys = [
            (x0, y0, z0), (x1, y0, z0), (x0, y0 + 8, z0), (x1, y0 + 8, z0),
            (x0, y0, z1), (x1, y0, z1), (x0, y0 + 8, z1), (x1, y0 + 8, z1),
        ];
        keys.into_iter().all(|(x, y, z)| {
            let value = self.eval::<false>(slope, x, y, z);
            (-65536.0..=-2.01).contains(&value)
        })
    }

    pub(crate) fn eval_overworld_final_density_cell_terrain_is_nonpositive(
        &mut self,
        plan: OverworldFinalDensityPlan,
        x0: i32,
        y0: i32,
        z0: i32,
    ) -> bool {
        debug_assert_eq!(self.geom.cell_width, 4);
        debug_assert_eq!(self.geom.cell_height, 8);
        debug_assert_eq!(x0.rem_euclid(self.geom.cell_width), 0);
        debug_assert_eq!(y0.rem_euclid(self.geom.cell_height), 0);
        debug_assert_eq!(z0.rem_euclid(self.geom.cell_width), 0);
        self.column_xz = None;
        self.column_y = None;
        self.tile_active = true;
        self.cell_xz = self
            .scratch
            .cell_column_indices(
                x0.div_euclid(self.geom.cell_width),
                z0.div_euclid(self.geom.cell_width),
            );

        let terrain = self.cell_corners(
            plan.terrain_inner,
            plan.terrain_slot,
            x0.div_euclid(self.geom.cell_width),
            y0.div_euclid(self.geom.cell_height),
            z0.div_euclid(self.geom.cell_width),
            self.geom.cell_width,
            self.geom.cell_height,
        );
        let result = terrain_subcell_is_strictly_nonpositive(
            self.geom.cell_width,
            self.geom.cell_height,
            terrain,
        );
        self.cell_xz = None;
        self.tile_active = false;
        result
    }

    fn noodle_subcell_is_positive(
        &mut self,
        plan: OverworldFinalDensityPlan,
        cx: i32,
        cy: i32,
        cz: i32,
    ) -> bool {
        let cw = self.geom.cell_width;
        let ch = self.geom.cell_height;
        if let Some((selector, roots)) = self.shared_noodle_ranges(plan) {
            let roots_present = roots.iter().any(|&(_, slot)| {
                self.scratch.cell_is_present(slot, cx, cy, cz, self.cell_xz)
            });
            if !roots_present {
                let mut selector_values = [0.0; 8];
                let mut selector_known = [false; 8];
                let control = self.cell_corners_shared_range(
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
                if noodle_control_is_inactive(cw, ch, control) {
                    return true;
                }
                if !subcell_values_are_finite(cw, ch, control) {
                    return false;
                }
                let ridge_a = self.cell_corners_shared_range(
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
                let ridge_b = self.cell_corners_shared_range(
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
                let thickness = self.cell_corners_shared_range(
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
                return noodle_active_lower_bound_is_positive(
                    cw,
                    ch,
                    ridge_a,
                    ridge_b,
                    thickness,
                );
            }
        }

        let control = self.cell_corners(
            plan.noodle_control_inner,
            plan.noodle_control_slot,
            cx,
            cy,
            cz,
            cw,
            ch,
        );
        if noodle_control_is_inactive(cw, ch, control) {
            return true;
        }
        if !subcell_values_are_finite(cw, ch, control) {
            return false;
        }
        let ridge_a = self.cell_corners(
            plan.noodle_ridge_a_inner,
            plan.noodle_ridge_a_slot,
            cx,
            cy,
            cz,
            cw,
            ch,
        );
        let ridge_b = self.cell_corners(
            plan.noodle_ridge_b_inner,
            plan.noodle_ridge_b_slot,
            cx,
            cy,
            cz,
            cw,
            ch,
        );
        let thickness = self.cell_corners(
            plan.noodle_thickness_inner,
            plan.noodle_thickness_slot,
            cx,
            cy,
            cz,
            cw,
            ch,
        );
        noodle_active_lower_bound_is_positive(cw, ch, ridge_a, ridge_b, thickness)
    }

    #[inline]
    pub(crate) fn eval<const INTERPOLATE: bool>(
        &mut self,
        id: NodeId,
        x: i32,
        y: i32,
        z: i32,
    ) -> f64 {
        let op = self.ops[id as usize];
        crate::counters::bump_density_eval(op.kind.density_kind());
        super::redundancy_probe::visit_field(
            std::ptr::from_ref(self.graph).cast::<()>(),
            id,
            op.kind.density_kind(),
            x,
            y,
            z,
            self.scratch.probe_scope(),
            INTERPOLATE,
        );
        if op.kind != OpKind::FlatCache && let Some(value) = self.product_value(id, x, z) {
            return value;
        }
        self.eval_op::<INTERPOLATE>(op, id, x, y, z)
    }

    #[inline(always)]
    fn eval_op<const INTERPOLATE: bool>(
        &mut self,
        op: Op,
        id: NodeId,
        x: i32,
        y: i32,
        z: i32,
    ) -> f64 {
        match op.kind {
            OpKind::Const => self.params[op.a as usize],
            OpKind::BlendAlpha => 1.0,
            OpKind::BlendOffset | OpKind::Beardifier => 0.0,
            OpKind::YClampedGradient => {
                let p = op.a as usize;
                crate::math::clamped_map(
                    f64::from(y),
                    self.params[p],
                    self.params[p + 1],
                    self.params[p + 2],
                    self.params[p + 3],
                )
            }
            OpKind::Add => {
                let left = self.eval::<INTERPOLATE>(op.a, x, y, z);
                self.eval_add_after_left::<INTERPOLATE>(left, op.b, x, y, z)
            }
            OpKind::Mul => {
                let left = self.eval::<INTERPOLATE>(op.a, x, y, z);
                if left == 0.0 {
                    0.0
                } else {
                    left * self.eval::<INTERPOLATE>(op.b, x, y, z)
                }
            }
            OpKind::Min => self
                .eval::<INTERPOLATE>(op.a, x, y, z)
                .min(self.eval::<INTERPOLATE>(op.b, x, y, z)),
            OpKind::Max => self
                .eval::<INTERPOLATE>(op.a, x, y, z)
                .max(self.eval::<INTERPOLATE>(op.b, x, y, z)),
            OpKind::Abs
            | OpKind::Square
            | OpKind::Cube
            | OpKind::HalfNegative
            | OpKind::QuarterNegative
            | OpKind::Squeeze
            | OpKind::Invert => self.eval_unary::<INTERPOLATE>(op, x, y, z),
            OpKind::Clamp => self.eval_clamp::<INTERPOLATE>(op, x, y, z),
            OpKind::Interpolated => self.eval_interpolated::<INTERPOLATE>(op, x, y, z),
            OpKind::FlatCache => self.eval_flat_cache(op, id, x, z),
            OpKind::Noise => self.eval_noise(op, id, x, y, z),
            OpKind::ShiftedNoise => self.eval_shifted_noise::<INTERPOLATE>(op, x, y, z),
            OpKind::ShiftA | OpKind::ShiftB | OpKind::Shift => self.eval_shift(op, x, y, z),
            OpKind::RangeChoice => self.eval_range_choice::<INTERPOLATE>(op, x, y, z),
            OpKind::DeepTerrainRangeChoice => self.eval_deep_terrain_range::<INTERPOLATE>(op, x, y, z),
            OpKind::IntervalSelect => self.eval_interval_select::<INTERPOLATE>(op, x, y, z),
            OpKind::Spline | OpKind::Blended | OpKind::FindTopSurface | OpKind::EndIslands => {
                self.eval_leaf(op, id, x, y, z)
            }
        }
    }

    #[inline(always)]
    fn eval_unary<const INTERPOLATE: bool>(
        &mut self,
        op: Op,
        x: i32,
        y: i32,
        z: i32,
    ) -> f64 {
        let value = self.eval::<INTERPOLATE>(op.a, x, y, z);
        match op.kind {
            OpKind::Abs => value.abs(),
            OpKind::Square => value * value,
            OpKind::Cube => value * value * value,
            OpKind::HalfNegative => if value > 0.0 { value } else { value * 0.5 },
            OpKind::QuarterNegative => if value > 0.0 { value } else { value * 0.25 },
            OpKind::Squeeze => {
                let value = value.clamp(-1.0, 1.0);
                value / 2.0 - value * value * value / 24.0
            }
            OpKind::Invert => 1.0 / value,
            _ => unreachable!(),
        }
    }

    #[inline(always)]
    fn eval_clamp<const INTERPOLATE: bool>(
        &mut self,
        op: Op,
        x: i32,
        y: i32,
        z: i32,
    ) -> f64 {
        let low = self.params[op.b as usize];
        let high = self.params[(op.b + 1) as usize];
        if let Some(maximum) = self.graph.clamp_max_rhs_first(op) {
            let right = self.eval::<INTERPOLATE>(maximum.b, x, y, z);
            if right > high {
                return high;
            }
            return self
                .eval::<INTERPOLATE>(maximum.a, x, y, z)
                .max(right)
                .clamp(low, high);
        }
        self.eval::<INTERPOLATE>(op.a, x, y, z).clamp(low, high)
    }

    #[inline(always)]
    fn eval_interpolated<const INTERPOLATE: bool>(
        &mut self,
        op: Op,
        x: i32,
        y: i32,
        z: i32,
    ) -> f64 {
        if INTERPOLATE {
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
            self.eval::<false>(op.a, x, y, z)
        }
    }

    #[inline(always)]
    fn eval_flat_cache(&mut self, op: Op, id: NodeId, x: i32, z: i32) -> f64 {
        let qx = (x >> 2) << 2;
        let qz = (z >> 2) << 2;
        if let Some(value) = self.product_value(id, qx, qz) {
            return value;
        }
        self.slot_get(op.b as usize, (qx, 0, qz), op.a)
    }

    #[inline(always)]
    fn eval_noise(&mut self, op: Op, id: NodeId, x: i32, y: i32, z: i32) -> f64 {
        if let Some(value) = self.scratch.leaf_get(id, x, y, z) {
            return value;
        }
        crate::counters::bump_cache_compute(crate::counters::CacheKind::Leaf);
        let noise = &self.noises[op.a as usize];
        let xz = self.params[op.b as usize];
        let ys = self.params[(op.b + 1) as usize];
        let value = noise.get_value(
            f64::from(x) * xz,
            f64::from(y) * ys,
            f64::from(z) * xz,
        );
        self.scratch.leaf_put(id, x, y, z, value);
        value
    }

    #[inline(always)]
    fn eval_shifted_noise<const INTERPOLATE: bool>(
        &mut self,
        op: Op,
        x: i32,
        y: i32,
        z: i32,
    ) -> f64 {
        let c0 = self.children[op.a as usize];
        let c1 = self.children[(op.a + 1) as usize];
        let c2 = self.children[(op.a + 2) as usize];
        let xz = self.params[op.c as usize];
        let ys = self.params[(op.c + 1) as usize];
        let sx = f64::from(x) * xz + self.eval::<INTERPOLATE>(c0, x, y, z);
        let sy = f64::from(y) * ys + self.eval::<INTERPOLATE>(c1, x, y, z);
        let sz = f64::from(z) * xz + self.eval::<INTERPOLATE>(c2, x, y, z);
        self.noises[op.b as usize].get_value(sx, sy, sz)
    }

    #[inline(always)]
    fn eval_shift(&self, op: Op, x: i32, y: i32, z: i32) -> f64 {
        let noise = &self.noises[op.a as usize];
        match op.kind {
            OpKind::ShiftA => shift(noise, f64::from(x), 0.0, f64::from(z)),
            OpKind::ShiftB => shift(noise, f64::from(z), f64::from(x), 0.0),
            OpKind::Shift => shift(noise, f64::from(x), f64::from(y), f64::from(z)),
            _ => unreachable!(),
        }
    }

    #[inline(always)]
    fn eval_add_after_left<const INTERPOLATE: bool>(
        &mut self, left: f64, right: NodeId, x: i32, y: i32, z: i32,
    ) -> f64 {
        left + self.eval::<INTERPOLATE>(right, x, y, z)
    }

    #[inline(always)]
    fn eval_range_choice<const INTERPOLATE: bool>(
        &mut self,
        op: Op,
        x: i32,
        y: i32,
        z: i32,
    ) -> f64 {
        let input = self.children[op.a as usize];
        let value = self.eval::<INTERPOLATE>(input, x, y, z);
        self.eval_range_value::<INTERPOLATE>(op, value, x, y, z)
    }

    #[inline(always)]
    fn eval_deep_terrain_range<const INTERPOLATE: bool>(
        &mut self, op: Op, x: i32, y: i32, z: i32,
    ) -> f64 {
        #[cfg(any(test, feature = "gen-counters"))]
        if !self.deep_saturation_enabled {
            return self.eval_range_choice::<INTERPOLATE>(op, x, y, z);
        }
        let plan = self.graph.deep_terrain_plan().expect("compiled deep terrain plan");
        let left = self.eval::<INTERPOLATE>(plan.non_blended, x, y, z);
        let admitted = plan.admits(left);
        #[cfg(test)]
        let admitted = admitted || self.force_deep_saturation;
        if admitted {
            return self.eval::<INTERPOLATE>(plan.residual, x, y, z);
        }
        let value = self.eval_add_after_left::<INTERPOLATE>(left, plan.blended, x, y, z);
        self.eval_range_value::<INTERPOLATE>(op, value, x, y, z)
    }

    #[inline(always)]
    fn eval_range_value<const INTERPOLATE: bool>(
        &mut self, op: Op, value: f64, x: i32, y: i32, z: i32,
    ) -> f64 {
        let branch = if value >= self.params[op.b as usize]
            && value < self.params[(op.b + 1) as usize]
        {
            self.children[(op.a + 1) as usize]
        } else {
            self.children[(op.a + 2) as usize]
        };
        self.eval::<INTERPOLATE>(branch, x, y, z)
    }

    #[inline(always)]
    fn eval_interval_select<const INTERPOLATE: bool>(
        &mut self,
        op: Op,
        x: i32,
        y: i32,
        z: i32,
    ) -> f64 {
        let count = self.children[op.a as usize];
        let input = self.children[(op.a + 1) as usize];
        let value = self.eval::<INTERPOLATE>(input, x, y, z);
        for index in 0..op.c {
            if value < self.params[(op.b + index) as usize] {
                let branch = self.children[(op.a + 2 + index) as usize];
                return self.eval::<INTERPOLATE>(branch, x, y, z);
            }
        }
        self.eval::<INTERPOLATE>(self.children[(op.a + 1 + count) as usize], x, y, z)
    }

    #[inline(always)]
    fn eval_leaf(&mut self, op: Op, id: NodeId, x: i32, y: i32, z: i32) -> f64 {
        if let Some(value) = self.scratch.leaf_get(id, x, y, z) {
            return value;
        }
        #[cfg(test)]
        if op.kind == OpKind::Blended && let Some(trace) = &mut self.blended_queries {
            trace.push((x, y, z));
        }
        #[cfg(feature = "gen-counters")]
        if op.kind == OpKind::Blended && y < 256
            && let Some(trace) = &mut self.blended_misses
        {
            trace.0.insert((x, y, z));
            trace.1 += 1;
        }
        crate::counters::bump_cache_compute(crate::counters::CacheKind::Leaf);
        let context = Context::new(x, y, z);
        let value = if op.kind == OpKind::Spline {
            self.splines[op.a as usize].compute(context, self.scratch.point_scratch_mut())
        } else if op.kind == OpKind::Blended
            && let Some(column) = &mut self.blended_column
            && let crate::density::Density::Blended(noise) = &self.leaves[op.a as usize]
            && let Some(value) = column.compute(id, noise, x, y, z)
        {
            value
        } else {
            self.leaves[op.a as usize].compute(context)
        };
        self.scratch.leaf_put(id, x, y, z, value);
        value
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
            OpKind::RangeChoice | OpKind::DeepTerrainRangeChoice => {
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
                selector_values[index] = self.eval::<false>(selector, key.0, key.1, key.2);
                selector_known[index] = true;
            }
            let value = self.eval_range_branch(range, selector_values[index], key);
            self.publish_slot(slot, key, value);
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
        crate::counters::bump_density_eval(op.kind.density_kind());
        super::redundancy_probe::visit_field(
            std::ptr::from_ref(self.graph).cast::<()>(),
            id,
            op.kind.density_kind(),
            x,
            y,
            z,
            self.scratch.probe_scope(),
            false,
        );
        let branch = if input >= self.params[op.b as usize]
            && input < self.params[(op.b + 1) as usize]
        {
            self.children[(op.a + 1) as usize]
        } else {
            self.children[(op.a + 2) as usize]
        };
        self.eval::<false>(branch, x, y, z)
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

        if self.tile_active && self.graph.tile_eligible(inner) {
            if let Some(plan) = self.graph.tile_plan(inner) {
                return self.eval_tile_plan_cell(plan, slot, cx, cy, cz, cw, ch);
            }
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

    fn eval_tile_plan_cell(
        &mut self,
        plan: &TilePlan,
        slot: usize,
        cx: i32,
        cy: i32,
        cz: i32,
        cw: i32,
        ch: i32,
    ) -> [f64; 8] {
        crate::counters::bump_cell_fill();
        crate::counters::bump_cache_compute(crate::counters::CacheKind::Cell);
        let keys = cell_corner_keys(cx, cy, cz, cw, ch);
        let mut values = [0.0; 8];
        let mut missing = 0_u8;
        for (lane, key) in keys.into_iter().enumerate() {
            crate::counters::bump_corner_lookup();
            if let Some(value) = self.scratch.slot_get(slot, key) {
                crate::counters::bump_slot_hit();
                values[lane] = value;
            } else {
                crate::counters::bump_slot_miss(slot);
                crate::counters::bump_corner_eval();
                crate::counters::bump_cache_compute(crate::counters::CacheKind::Slot);
                missing |= 1 << lane;
            }
        }
        if missing != 0 {
            let computed = self.eval_tile_plan(plan, keys, missing);
            for (lane, key) in keys.into_iter().enumerate() {
                if missing & (1 << lane) != 0 {
                    values[lane] = computed[lane];
                    self.publish_slot(slot, key, computed[lane]);
                }
            }
        }
        self.scratch.cell_put(slot, cx, cy, cz, values);
        values
    }

    fn eval_tile_plan(
        &mut self,
        plan: &TilePlan,
        contexts: [(i32, i32, i32); 8],
        active: u8,
    ) -> [f64; 8] {
        eval_tile_plan(
            plan,
            &contexts,
            active,
            self.scratch,
            self.noises,
            self.leaves,
            self.products,
            std::ptr::from_ref(self.graph),
        )
    }

    fn eval_tile_plan_roots(
        &mut self,
        plan: &TilePlan,
        roots: [u16; 5],
        contexts: [(i32, i32, i32); 8],
        active: [u8; 5],
    ) -> [[f64; 8]; 5] {
        self.scratch.begin_tile_plan(plan.ops.len());
        for root in 0..5 {
            if active[root] != 0 {
                eval_tile_plan_node(
                    plan,
                    roots[root],
                    &contexts,
                    active[root],
                    self.scratch,
                    self.noises,
                    self.leaves,
                    self.products,
                    std::ptr::from_ref(self.graph),
                );
            }
        }
        let mut values = [[0.0; 8]; 5];
        for root in 0..5 {
            values[root] = self.scratch.plan_values(roots[root]);
        }
        values
    }

    #[inline(always)]
    fn product_value(&self, id: NodeId, x: i32, z: i32) -> Option<f64> {
        self.products?.get(self.graph.product_kind(id)?, x, z)
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
        let v = self.eval::<false>(inner, x, y, z);
        self.publish_slot(slot, key, v);
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
        let v = self.eval::<false>(inner, key.0, key.1, key.2);
        self.publish_slot(slot, key, v);
        v
    }

    #[inline]
    fn publish_slot(&mut self, slot: usize, key: (i32, i32, i32), value: f64) {
        self.scratch.slot_put(slot, key, value);
        #[cfg(test)]
        if let Some(publications) = &mut self.publications {
            publications.push((slot, key, value.to_bits()));
        }
    }
}

fn squeeze_column(output: &mut [f64]) {
    dispatch!(Level::new(), simd => squeeze_column_kernel(simd, output));
}

#[simd]
fn squeeze_column_kernel<S: Simd>(simd: S, output: &mut [f64]) {
    let mut groups = output.chunks_exact_mut(8);
    for group in &mut groups {
        let clamped = std::array::from_fn(|lane| group[lane].clamp(-1.0, 1.0));
        let clamped = f64x8::simd_from(simd, clamped);
        let cubed = clamped * clamped * clamped;
        let squeezed = clamped / f64x8::splat(simd, 2.0) - cubed / f64x8::splat(simd, 24.0);
        group.copy_from_slice(&squeezed.to_array());
    }
    for value in groups.into_remainder() {
        let clamped = value.clamp(-1.0, 1.0);
        *value = clamped / 2.0 - clamped * clamped * clamped / 24.0;
    }
}

fn eval_tile_plan(
    plan: &TilePlan,
    contexts: &[(i32, i32, i32); 8],
    active: u8,
    scratch: &mut Scratch,
    noises: &[crate::noise::NormalNoise],
    leaves: &[crate::density::Density],
    products: Option<&XzProductLattice>,
    graph: *const Graph,
) -> [f64; 8] {
    scratch.begin_tile_plan(plan.ops.len());
    eval_tile_plan_node(
        plan, plan.root, contexts, active, scratch, noises, leaves, products, graph,
    );
    scratch.plan_values(plan.root)
}

fn eval_tile_plan_node(
    plan: &TilePlan,
    reg: u16,
    contexts: &[(i32, i32, i32); 8],
    active: u8,
    scratch: &mut Scratch,
    noises: &[crate::noise::NormalNoise],
    leaves: &[crate::density::Density],
    products: Option<&XzProductLattice>,
    graph: *const Graph,
) {
    let missing = scratch.plan_missing(reg, active);
    if missing == 0 {
        return;
    }
    let op = plan.ops[reg as usize];
    for lane in 0..8 {
        if missing & (1 << lane) != 0 {
            crate::counters::bump_density_eval(op.kind.density_kind());
            let (x, y, z) = contexts[lane];
            super::redundancy_probe::visit_field_tile(
                graph.cast::<()>(),
                op.source,
                op.kind.density_kind(),
                x,
                y,
                z,
                scratch.probe_scope(),
            );
        }
    }
    if op.product != 0 {
        let kind = if op.product == 1 {
            super::xz_products::XzProductKind::Factor
        } else {
            super::xz_products::XzProductKind::Offset
        };
        let mut values = [0.0; 8];
        let mut product_mask = 0_u8;
        if let Some(products) = products {
            for lane in 0..8 {
                if missing & (1 << lane) == 0 {
                    continue;
                }
                let (x, _, z) = contexts[lane];
                if let Some(value) = products.get(kind, x, z) {
                    product_mask |= 1 << lane;
                    values[lane] = value;
                }
            }
        }
        scratch.plan_put(reg, product_mask, values);
    }
    let missing = scratch.plan_missing(reg, missing);
    if missing == 0 {
        return;
    }
    let mut values = [0.0; 8];
    match op.kind {
        OpKind::Const => values.fill(plan.params[op.params as usize]),
        OpKind::BlendAlpha => values.fill(1.0),
        OpKind::BlendOffset | OpKind::Beardifier => {}
        OpKind::YClampedGradient => {
            let p = op.params as usize;
            for lane in 0..8 {
                if missing & (1 << lane) != 0 {
                    values[lane] = crate::math::clamped_map(
                        f64::from(contexts[lane].1),
                        plan.params[p],
                        plan.params[p + 1],
                        plan.params[p + 2],
                        plan.params[p + 3],
                    );
                }
            }
        }
        OpKind::Add | OpKind::Min | OpKind::Max => {
            eval_tile_plan_node(
                plan, op.a, contexts, missing, scratch, noises, leaves, products, graph,
            );
            eval_tile_plan_node(
                plan, op.b, contexts, missing, scratch, noises, leaves, products, graph,
            );
            for lane in 0..8 {
                if missing & (1 << lane) != 0 {
                    let left = scratch.plan_value(op.a, lane);
                    let right = scratch.plan_value(op.b, lane);
                    values[lane] = match op.kind {
                        OpKind::Add => left + right,
                        OpKind::Min => left.min(right),
                        OpKind::Max => left.max(right),
                        _ => unreachable!(),
                    };
                }
            }
        }
        OpKind::Mul => {
            eval_tile_plan_node(
                plan, op.a, contexts, missing, scratch, noises, leaves, products, graph,
            );
            let mut right = 0_u8;
            for lane in 0..8 {
                if missing & (1 << lane) != 0 && scratch.plan_value(op.a, lane) != 0.0 {
                    right |= 1 << lane;
                }
            }
            eval_tile_plan_node(
                plan, op.b, contexts, right, scratch, noises, leaves, products, graph,
            );
            for lane in 0..8 {
                if missing & (1 << lane) != 0 {
                    let left = scratch.plan_value(op.a, lane);
                    values[lane] = if left == 0.0 {
                        0.0
                    } else {
                        left * scratch.plan_value(op.b, lane)
                    };
                }
            }
        }
        OpKind::Abs
        | OpKind::Square
        | OpKind::Cube
        | OpKind::HalfNegative
        | OpKind::QuarterNegative
        | OpKind::Squeeze
        | OpKind::Invert
        | OpKind::Clamp => {
            eval_tile_plan_node(
                plan, op.a, contexts, missing, scratch, noises, leaves, products, graph,
            );
            let p = op.params as usize;
            for lane in 0..8 {
                if missing & (1 << lane) == 0 {
                    continue;
                }
                let value = scratch.plan_value(op.a, lane);
                values[lane] = match op.kind {
                    OpKind::Abs => value.abs(),
                    OpKind::Square => value * value,
                    OpKind::Cube => value * value * value,
                    OpKind::HalfNegative => {
                        if value > 0.0 { value } else { value * 0.5 }
                    }
                    OpKind::QuarterNegative => {
                        if value > 0.0 { value } else { value * 0.25 }
                    }
                    OpKind::Squeeze => {
                        let value = value.clamp(-1.0, 1.0);
                        value / 2.0 - value * value * value / 24.0
                    }
                    OpKind::Invert => 1.0 / value,
                    OpKind::Clamp => crate::math::clamp(value, plan.params[p], plan.params[p + 1]),
                    _ => unreachable!(),
                };
            }
        }
        OpKind::Noise => {
            let noise = &noises[op.aux as usize];
            let p = op.params as usize;
            for lane in 0..8 {
                if missing & (1 << lane) == 0 {
                    continue;
                }
                let (x, y, z) = contexts[lane];
                values[lane] = if let Some(value) = scratch.leaf_get(op.source, x, y, z) {
                    value
                } else {
                    crate::counters::bump_cache_compute(crate::counters::CacheKind::Leaf);
                    let value = noise.get_value(
                        f64::from(x) * plan.params[p],
                        f64::from(y) * plan.params[p + 1],
                        f64::from(z) * plan.params[p],
                    );
                    scratch.leaf_put(op.source, x, y, z, value);
                    value
                };
            }
        }
        OpKind::ShiftedNoise => {
            eval_tile_plan_node(
                plan, op.a, contexts, missing, scratch, noises, leaves, products, graph,
            );
            eval_tile_plan_node(
                plan, op.b, contexts, missing, scratch, noises, leaves, products, graph,
            );
            eval_tile_plan_node(
                plan, op.c, contexts, missing, scratch, noises, leaves, products, graph,
            );
            let noise = &noises[op.aux as usize];
            let p = op.params as usize;
            for lane in 0..8 {
                if missing & (1 << lane) != 0 {
                    let (x, y, z) = contexts[lane];
                    values[lane] = noise.get_value(
                        f64::from(x) * plan.params[p] + scratch.plan_value(op.a, lane),
                        f64::from(y) * plan.params[p + 1] + scratch.plan_value(op.b, lane),
                        f64::from(z) * plan.params[p] + scratch.plan_value(op.c, lane),
                    );
                }
            }
        }
        OpKind::ShiftA | OpKind::ShiftB | OpKind::Shift => {
            let noise = &noises[op.aux as usize];
            for lane in 0..8 {
                if missing & (1 << lane) != 0 {
                    let (x, y, z) = contexts[lane];
                    values[lane] = match op.kind {
                        OpKind::ShiftA => shift(noise, f64::from(x), 0.0, f64::from(z)),
                        OpKind::ShiftB => shift(noise, f64::from(z), f64::from(x), 0.0),
                        OpKind::Shift => shift(noise, f64::from(x), f64::from(y), f64::from(z)),
                        _ => unreachable!(),
                    };
                }
            }
        }
        OpKind::RangeChoice => {
            eval_tile_plan_node(
                plan, op.a, contexts, missing, scratch, noises, leaves, products, graph,
            );
            let p = op.params as usize;
            let mut inside = 0_u8;
            let mut outside = 0_u8;
            for lane in 0..8 {
                if missing & (1 << lane) == 0 {
                    continue;
                }
                if scratch.plan_value(op.a, lane) >= plan.params[p]
                    && scratch.plan_value(op.a, lane) < plan.params[p + 1]
                {
                    inside |= 1 << lane;
                } else {
                    outside |= 1 << lane;
                }
            }
            eval_tile_plan_node(
                plan, op.b, contexts, inside, scratch, noises, leaves, products, graph,
            );
            eval_tile_plan_node(
                plan, op.c, contexts, outside, scratch, noises, leaves, products, graph,
            );
            for lane in 0..8 {
                if missing & (1 << lane) != 0 {
                    let child = if inside & (1 << lane) != 0 { op.b } else { op.c };
                    values[lane] = scratch.plan_value(child, lane);
                }
            }
        }
        OpKind::IntervalSelect => {
            eval_tile_plan_node(
                plan, op.a, contexts, missing, scratch, noises, leaves, products, graph,
            );
            for lane in 0..8 {
                if missing & (1 << lane) == 0 {
                    continue;
                }
                let value = scratch.plan_value(op.a, lane);
                let mut selected = op.branch_count;
                let p = op.params as usize;
                for index in 0..usize::from(op.threshold_count) {
                    if value < plan.params[p + index] {
                        selected = index as u16;
                        break;
                    }
                }
                scratch.plan_set_select(reg, lane, selected);
            }
            for index in 0..usize::from(op.branch_count) {
                let branch_mask = scratch.plan_branch_mask(
                    reg,
                    missing,
                    index as u16,
                    op.branch_count,
                );
                if branch_mask != 0 {
                    eval_tile_plan_node(
                        plan,
                        plan.branches[op.aux as usize + index],
                        contexts,
                        branch_mask,
                        scratch,
                        noises,
                        leaves,
                        products,
                        graph,
                    );
                }
            }
            for lane in 0..8 {
                if missing & (1 << lane) != 0 {
                    let index = usize::from(scratch.plan_select(reg, lane))
                        .min(usize::from(op.branch_count.saturating_sub(1)));
                    values[lane] = scratch.plan_value(plan.branches[op.aux as usize + index], lane);
                }
            }
        }
        OpKind::EndIslands => {
            let leaf = &leaves[op.aux as usize];
            for lane in 0..8 {
                if missing & (1 << lane) == 0 {
                    continue;
                }
                let (x, y, z) = contexts[lane];
                values[lane] = if let Some(value) = scratch.leaf_get(op.source, x, y, z) {
                    value
                } else {
                    crate::counters::bump_cache_compute(crate::counters::CacheKind::Leaf);
                    let value = leaf.compute(Context::new(x, y, z));
                    scratch.leaf_put(op.source, x, y, z, value);
                    value
                };
            }
        }
        OpKind::Interpolated
        | OpKind::FlatCache
        | OpKind::Spline
        | OpKind::Blended
        | OpKind::FindTopSurface
        | OpKind::DeepTerrainRangeChoice => {
            unreachable!("cache-bearing node entered compiled tile plan")
        }
    }
    scratch.plan_put(reg, missing, values);
}

fn cell_corner_keys(
    cx: i32,
    cy: i32,
    cz: i32,
    cw: i32,
    ch: i32,
) -> [(i32, i32, i32); 8] {
    let (x0, y0, z0) = (cx * cw, cy * ch, cz * cw);
    let (x1, y1, z1) = (x0 + cw, y0 + ch, z0 + cw);
    [
        (x0, y0, z0),
        (x1, y0, z0),
        (x0, y1, z0),
        (x1, y1, z0),
        (x0, y0, z1),
        (x1, y0, z1),
        (x0, y1, z1),
        (x1, y1, z1),
    ]
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
    dispatch!(Level::new(), simd => write_interpolated_corners_kernel(simd, mask, n, output));
}

#[simd]
fn write_interpolated_corners_kernel<S: Simd>(
    simd: S,
    mask: [u64; 2],
    n: [f64; 8],
    output: &mut [f64; 128],
) {
    for index in (0..128).step_by(8) {
        let values = interpolate_canonical_chunk(simd, index, &n).to_array();
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
    dispatch!(Level::new(), simd => write_shared_noodle_output_kernel(simd, mask, channels, output));
}

#[simd]
fn write_shared_noodle_output_kernel<S: Simd>(
    simd: S,
    mask: [u64; 2],
    channels: &[[f64; 8]; 4],
    output: &mut [f64; 128],
) {
    for index in (0..128).step_by(8) {
        let mut clamped = [0.0; 8];
        for lane in 0..8 {
            clamped[lane] = output[index + lane].clamp(-1.0, 1.0);
        }
        let clamped = f64x8::simd_from(simd, clamped);
        let squared = clamped * clamped;
        let cubed = squared * clamped;
        let squeezed = clamped / f64x8::splat(simd, 2.0) - cubed / f64x8::splat(simd, 24.0);
        let squeezed = squeezed.to_array();

        let group_mask = (mask[index >> 6] >> (index & 63)) & 0xff;
        if group_mask == 0 {
            for lane in 0..8 {
                output[index + lane] = squeezed[lane].min(64.0);
            }
            continue;
        }

        let ridge_a = interpolate_canonical_chunk(simd, index, &channels[1]).to_array();
        let ridge_b = interpolate_canonical_chunk(simd, index, &channels[2]).to_array();
        let mut active = [false; 8];
        let mut max_abs = [0.0; 8];
        for lane in 0..8 {
            active[lane] = mask_contains(mask, index + lane);
            if active[lane] {
                max_abs[lane] = ridge_a[lane].abs().max(ridge_b[lane].abs());
            }
        }
        let thickness = interpolate_canonical_chunk(simd, index, &channels[3]);
        let noodle = (thickness + f64x8::splat(simd, 1.5) * f64x8::simd_from(simd, max_abs)).to_array();
        for lane in 0..8 {
            if active[lane] {
                output[index + lane] = squeezed[lane].min(noodle[lane]);
            } else {
                output[index + lane] = squeezed[lane].min(64.0);
            }
        }
    }
}

#[inline(always)]
fn interpolate_canonical_chunk<S: Simd>(simd: S, start: usize, n: &[f64; 8]) -> f64x8<S> {
    const XZ_FACTORS: [f64; 4] = [0.0, 0.25, 0.5, 0.75];
    const Y_FACTORS: [f64; 8] = [0.0, 0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875];
    debug_assert!(start < 128 && start % 8 == 0);
    let group = start / 8;
    let x = f64x8::splat(simd, XZ_FACTORS[group % 4]);
    let y = f64x8::simd_from(simd, Y_FACTORS);
    let z = f64x8::splat(simd, XZ_FACTORS[group / 4]);
    let p0 = f64x8::splat(simd, n[0]) + x * (f64x8::splat(simd, n[1]) - f64x8::splat(simd, n[0]));
    let p1 = f64x8::splat(simd, n[2]) + x * (f64x8::splat(simd, n[3]) - f64x8::splat(simd, n[2]));
    let p2 = f64x8::splat(simd, n[4]) + x * (f64x8::splat(simd, n[5]) - f64x8::splat(simd, n[4]));
    let p3 = f64x8::splat(simd, n[6]) + x * (f64x8::splat(simd, n[7]) - f64x8::splat(simd, n[6]));
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

/// The eight vertices of the rectangular block region actually emitted by a
/// 4×8×4 cell. The enclosing cell's far face is not sampled: the last block is
/// at fractions 3/4, 7/8, 3/4.
#[inline]
fn subcell_corners(cw: i32, ch: i32, n: [f64; 8]) -> [f64; 8] {
    let x = f64::from(cw - 1) / f64::from(cw);
    let y = f64::from(ch - 1) / f64::from(ch);
    let z = x;
    [
        crate::math::lerp3(0.0, 0.0, 0.0, n[0], n[1], n[2], n[3], n[4], n[5], n[6], n[7]),
        crate::math::lerp3(x, 0.0, 0.0, n[0], n[1], n[2], n[3], n[4], n[5], n[6], n[7]),
        crate::math::lerp3(0.0, y, 0.0, n[0], n[1], n[2], n[3], n[4], n[5], n[6], n[7]),
        crate::math::lerp3(x, y, 0.0, n[0], n[1], n[2], n[3], n[4], n[5], n[6], n[7]),
        crate::math::lerp3(0.0, 0.0, z, n[0], n[1], n[2], n[3], n[4], n[5], n[6], n[7]),
        crate::math::lerp3(x, 0.0, z, n[0], n[1], n[2], n[3], n[4], n[5], n[6], n[7]),
        crate::math::lerp3(0.0, y, z, n[0], n[1], n[2], n[3], n[4], n[5], n[6], n[7]),
        crate::math::lerp3(x, y, z, n[0], n[1], n[2], n[3], n[4], n[5], n[6], n[7]),
    ]
}

#[inline]
fn terrain_subcell_is_positive(cw: i32, ch: i32, n: [f64; 8]) -> bool {
    subcell_corners(cw, ch, n).into_iter().all(|value| {
        if !value.is_finite() {
            return false;
        }
        let squeezed = value.clamp(-1.0, 1.0);
        let squeezed = squeezed / 2.0 - squeezed * squeezed * squeezed / 24.0;
        squeezed.is_normal() && squeezed > 0.0
    })
}

#[inline]
fn terrain_subcell_is_strictly_nonpositive(cw: i32, ch: i32, n: [f64; 8]) -> bool {
    if n.iter().any(|value| !value.is_finite()) {
        return false;
    }
    let scale = n.iter().fold(1.0_f64, |scale, value| scale.max(value.abs()));
    if scale > f64::MAX / 4.0 {
        return false;
    }
    let margin = scale * (128.0 * f64::EPSILON) + 128.0 * f64::MIN_POSITIVE;
    subcell_corners(cw, ch, n)
        .into_iter()
        .all(|value| value.is_finite() && value < -margin)
}

#[inline]
fn noodle_control_is_inactive(cw: i32, ch: i32, n: [f64; 8]) -> bool {
    let values = subcell_corners(cw, ch, n);
    values.iter().all(|value| value.is_finite())
        && values.iter().all(|value| *value > -1_000_000.0)
        && values
            .iter()
            .all(|value| *value <= -f64::MIN_POSITIVE)
}

#[inline]
fn subcell_values_are_finite(cw: i32, ch: i32, n: [f64; 8]) -> bool {
    subcell_corners(cw, ch, n)
        .iter()
        .all(|value| value.is_finite())
}

#[inline]
fn positive_abs_lower_bound(values: [f64; 8]) -> Option<f64> {
    if values.iter().any(|value| !value.is_finite()) {
        return None;
    }
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if min > 0.0 {
        Some(min)
    } else if max < 0.0 {
        Some(-max)
    } else {
        Some(0.0)
    }
}

#[inline]
fn noodle_active_lower_bound_is_positive(
    cw: i32,
    ch: i32,
    ridge_a: [f64; 8],
    ridge_b: [f64; 8],
    thickness: [f64; 8],
) -> bool {
    let thickness = subcell_corners(cw, ch, thickness);
    if thickness.iter().any(|value| !value.is_finite()) {
        return false;
    }
    let ridge_a = positive_abs_lower_bound(subcell_corners(cw, ch, ridge_a));
    let ridge_b = positive_abs_lower_bound(subcell_corners(cw, ch, ridge_b));
    let (Some(ridge_a), Some(ridge_b)) = (ridge_a, ridge_b) else {
        return false;
    };
    let thickness_min = thickness.into_iter().fold(f64::INFINITY, f64::min);
    let lower = thickness_min + 1.5 * ridge_a.max(ridge_b);
    lower.is_normal() && lower > 0.0
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
        squeeze_column, squeeze_column_kernel,
        terrain_subcell_is_strictly_nonpositive, write_interpolated_corners,
        write_interpolated_corners_kernel, write_interpolated_corners_scalar,
        write_shared_noodle_output, write_shared_noodle_output_kernel,
        write_shared_noodle_output_scalar,
    };
    use super::Field;
    #[cfg(feature = "gen-counters")]
    use crate::counters;
    use crate::density::Density;
    use crate::engine::{Bounds, Geom, Program, Scratch};

    fn squeeze(value: f64) -> f64 {
        let value = value.clamp(-1.0, 1.0);
        value / 2.0 - value * value * value / 24.0
    }

    fn interpolate_eight(n: [f64; 8], x: f64, y: f64, z: f64) -> f64 {
        let lerp = |t: f64, a: f64, b: f64| a + t * (b - a);
        let low = lerp(y, lerp(x, n[0], n[1]), lerp(x, n[2], n[3]));
        let high = lerp(y, lerp(x, n[4], n[5]), lerp(x, n[6], n[7]));
        lerp(z, low, high)
    }

    #[test]
    fn squeezed_column_uses_the_interpolated_eight_corner_value() {
        let corners = [-2.75, 0.125, 0.625, -0.5, 3.125, -0.875, 0.25, 1.75];
        let program = Program::compile(&Density::Squeeze(Box::new(Density::Interpolated {
            inner: Box::new(Density::Const(0.0)),
            slot: 0,
        })));
        for (cw, ch) in [(4, 8), (8, 4)] {
            let mut scratch = Scratch::acquire(1, cw, ch, None);
            for (index, corner) in corners.into_iter().enumerate() {
                let key = (
                    -cw + (index as i32 % 2) * cw,
                    -ch + ((index as i32 / 2) % 2) * ch,
                    -cw + (index as i32 / 4) * cw,
                );
                scratch.slot_put(0, key, corner);
            }
            let mut field = Field::new(
                program.graph(),
                Geom {
                    cell_width: cw,
                    cell_height: ch,
                },
                &mut scratch,
            );
            let mut output = [0.0; 1];
            field.eval_column(program.root(), -cw + 1, -1, -ch + 3, &mut output);
            let expected = squeeze(interpolate_eight(
                corners,
                1.0 / f64::from(cw),
                3.0 / f64::from(ch),
                f64::from(cw - 1) / f64::from(cw),
            ));
            assert_eq!(output[0].to_bits(), expected.to_bits(), "geometry {cw}x{ch}");
        }
    }

    #[test]
    #[should_panic(expected = "squeeze must follow interpolation")]
    fn squeezed_corner_interpolation_is_rejected_by_fractional_control() {
        let corners = [-2.75, 0.125, 0.625, -0.5, 3.125, -0.875, 0.25, 1.75];
        let expected = squeeze(interpolate_eight(corners, 0.25, 0.375, 0.75));
        let wrong = interpolate_eight(corners.map(squeeze), 0.25, 0.375, 0.75);
        assert_eq!(
            wrong.to_bits(), expected.to_bits(), "squeeze must follow interpolation",
        );
    }

    #[test]
    fn squeezed_column_simd_and_tail_preserve_special_value_bits() {
        let values = [
            -0.0, 0.0, -0.125, 0.1875, -1.0, 1.0, -7.5, 9.25,
            f64::NEG_INFINITY, f64::INFINITY, f64::NAN,
            f64::from_bits(0xfff8_0000_0000_1234),
            f64::from_bits(1), -f64::from_bits(1), -0.75, 0.625,
            -0.0, f64::from_bits(0x7ff8_0000_0000_4321), f64::INFINITY,
        ];
        let mut actual = values;
        let mut fallback = values;
        squeeze_column(&mut actual);
        squeeze_column_kernel(fearless_simd::Fallback::new(), &mut fallback);
        for (index, value) in values.into_iter().enumerate() {
            let expected = squeeze(value).to_bits();
            assert_eq!(actual[index].to_bits(), expected, "native lane {index}");
            assert_eq!(fallback[index].to_bits(), expected, "fallback lane {index}");
        }
    }

    #[test]
    fn squeezed_columns_match_scalar_bits_and_slot_publications() {
        let interpolated = Density::Interpolated {
            inner: Box::new(Density::Add(
                Box::new(Density::Cube(Box::new(Density::YClampedGradient {
                    from_y: -32.0,
                    to_y: 32.0,
                    from_value: -2.0,
                    to_value: 2.0,
                }))),
                Box::new(Density::FlatCache {
                    inner: Box::new(Density::Const(0.1875)),
                    slot: 1,
                    memo: crate::density::XzMemoId::NONE,
                }),
            )),
            slot: 0,
        };
        let roots = [
            Density::Squeeze(Box::new(interpolated.clone())),
            Density::Add(
                Box::new(Density::Squeeze(Box::new(interpolated))),
                Box::new(Density::Const(-0.0)),
            ),
        ];
        for root in roots {
            let program = Program::compile(&root);
            for (cw, ch) in [(4, 8), (8, 4)] {
                for bounds in [
                    None,
                    Some(Bounds {
                        x: (-16, 15),
                        y: (-24, 31),
                        z: (-16, 15),
                    }),
                ] {
                    let geom = Geom {
                        cell_width: cw,
                        cell_height: ch,
                    };
                    let mut column_scratch = Scratch::acquire(2, cw, ch, bounds);
                    let mut scalar_scratch = Scratch::acquire(2, cw, ch, bounds);
                    let mut column = Field::new(program.graph(), geom, &mut column_scratch);
                    let mut scalar = Field::new(program.graph(), geom, &mut scalar_scratch);
                    column.publications = Some(Vec::new());
                    scalar.publications = Some(Vec::new());
                    for (x, z) in [(-7, 9), (5, -11), (0, 0), (-7, 9)] {
                        let mut output = [0.0; 31];
                        column.eval_column(program.root(), x, z, -15, &mut output);
                        for (offset, got) in output.into_iter().enumerate() {
                            let y = -15 + offset as i32;
                            let expected = scalar.eval::<true>(program.root(), x, y, z);
                            assert_eq!(
                                got.to_bits(), expected.to_bits(), "({x},{y},{z}), {cw}x{ch}",
                            );
                        }
                    }
                    assert!(!column.publications.as_ref().unwrap().is_empty());
                    assert_eq!(column.publications, scalar.publications, "geometry {cw}x{ch}");
                }
            }
        }
    }

    #[test]
    fn column_root_slot_invalidation_preserves_noise_and_split_runs() {
        let mut random = crate::rng::XoroshiroRandomSource::new(17);
        let noise = Density::Noise {
            noise: crate::noise::NormalNoise::create(&mut random, -3, &[1.0, 0.5]),
            xz_scale: 0.375,
            y_scale: 0.625,
        };
        let interpolated = Density::Interpolated {
            inner: Box::new(noise.clone()),
            slot: 37,
        };
        let roots = [
            (interpolated.clone(), true),
            (Density::Squeeze(Box::new(interpolated.clone())), true),
            (Density::Add(
                Box::new(interpolated),
                Box::new(Density::Interpolated {
                    inner: Box::new(Density::Add(
                        Box::new(noise),
                        Box::new(Density::Const(0.375)),
                    )),
                    slot: 5,
                }),
            ), false),
        ];
        for (root, retained) in roots {
            let program = Program::compile(&root);
            for (cw, ch) in [(4, 8), (8, 4)] {
                let geom = Geom { cell_width: cw, cell_height: ch };
                for bounds in [None, Some(Bounds {
                    x: (-16, 15), y: (-24, 31), z: (-16, 15),
                })] {
                    let mut actual_scratch = Scratch::acquire(4096, cw, ch, bounds);
                    let mut scalar_scratch = Scratch::acquire(38, cw, ch, bounds);
                    actual_scratch.put_column_values(4095, 0, vec![123.5; ch as usize]);
                    {
                        let mut actual = Field::new(program.graph(), geom, &mut actual_scratch);
                        let mut scalar = Field::new(program.graph(), geom, &mut scalar_scratch);
                        for (x, z) in [(-7, 9), (5, -11), (-6, 10), (-7, 9)] {
                            for (start, len) in [(-15, 3), (-10, 2), (-7, 11), (6, 9)] {
                                let mut output = [0.0; 11];
                                actual.eval_column(program.root(), x, z, start, &mut output[..len]);
                                for (offset, got) in output[..len].iter().enumerate() {
                                    let y = start + offset as i32;
                                    let expected = scalar.eval::<true>(program.root(), x, y, z);
                                    assert_eq!(
                                        got.to_bits(), expected.to_bits(),
                                        "column root mismatch at ({x},{y},{z}), {cw}x{ch}",
                                    );
                                }
                            }
                        }
                    }
                    assert_eq!(
                        actual_scratch.column_value(4095, 0, 2),
                        retained.then_some(123.5),
                        "only admitted roots may retain an unused column tag",
                    );
                }
            }
        }
    }

    #[test]
    #[should_panic(expected = "stale root column must be invalidated")]
    fn column_root_slot_invalidation_rejects_stale_root_control() {
        let program = Program::compile(&Density::Interpolated {
            inner: Box::new(Density::YClampedGradient {
                from_y: -8.0, to_y: 8.0, from_value: -0.75, to_value: 0.875,
            }),
            slot: 37,
        });
        let geom = Geom { cell_width: 4, cell_height: 8 };
        let mut scratch = Scratch::acquire(38, 4, 8, None);
        scratch.put_column_values(37, 0, vec![123.5; 8]);
        let mut field = Field::new(program.graph(), geom, &mut scratch);
        let mut output = [0.0; 1];
        field.eval_interpolated_column(
            program.graph().op(program.root()), 1, -3, 2, &mut output,
        );
        let expected: f64 = -0.75 + (10.0 / 16.0) * (0.875 + 0.75);
        assert_eq!(
            output[0].to_bits(), expected.to_bits(),
            "stale root column must be invalidated",
        );
    }

    #[test]
    fn terrain_nonpositive_proof_rejects_ambiguous_and_nonfinite_corners() {
        assert!(terrain_subcell_is_strictly_nonpositive(4, 8, [-1.0; 8]));
        assert!(!terrain_subcell_is_strictly_nonpositive(4, 8, [-1.0e-15; 8]));
        assert!(!terrain_subcell_is_strictly_nonpositive(4, 8, [0.0; 8]));
        assert!(!terrain_subcell_is_strictly_nonpositive(
            4,
            8,
            [-1.0, -1.0, -1.0, -1.0, -1.0, -1.0, -1.0, f64::NAN]
        ));
        assert!(!terrain_subcell_is_strictly_nonpositive(
            4,
            8,
            [1.0, -1.0, -1.0, -1.0, -1.0, -1.0, -1.0, -1.0]
        ));
    }

    #[test]
    fn pure_tile_walk_preserves_branch_bits_and_negative_zero() {
        let varying = Density::YClampedGradient {
            from_y: -1.0,
            to_y: 1.0,
            from_value: -1.0,
            to_value: 1.0,
        };
        let root = Density::Add(
            Box::new(Density::Mul(
                Box::new(varying.clone()),
                Box::new(Density::Const(f64::NAN)),
            )),
            Box::new(Density::RangeChoice {
                input: Box::new(varying),
                min_inclusive: -0.5,
                max_exclusive: 0.5,
                when_in_range: Box::new(Density::Const(-0.0)),
                when_out_of_range: Box::new(Density::Const(2.0)),
            }),
        );
        let program = Program::compile(&root);
        assert!(program.graph().tile_eligible(program.root()));
        let contexts = [
            (-1, -1, 0),
            (0, -1, 0),
            (1, -1, 0),
            (0, 0, 0),
            (1, 0, 0),
            (0, 1, 0),
            (1, 1, 0),
            (-1, 1, 0),
        ];

        let mut tile_scratch = Scratch::acquire(0, 4, 8, None);
        let mut tile_field = Field::new(
            program.graph(),
            Geom {
                cell_width: 4,
                cell_height: 8,
            },
            &mut tile_scratch,
        );
        let tile_plan = program.graph().compile_tile_plan_for_test(program.root());
        let actual = tile_field.eval_tile_plan(&tile_plan, contexts, u8::MAX);

        let mut scalar_scratch = Scratch::acquire(0, 4, 8, None);
        let mut scalar_field = Field::new(
            program.graph(),
            Geom {
                cell_width: 4,
                cell_height: 8,
            },
            &mut scalar_scratch,
        );
        for (lane, (x, y, z)) in contexts.into_iter().enumerate() {
            let expected = scalar_field.eval::<false>(program.root(), x, y, z);
            assert_eq!(actual[lane].to_bits(), expected.to_bits(), "lane {lane}");
        }
        assert!(actual[0].is_nan());

        let branch = Density::RangeChoice {
            input: Box::new(Density::Const(0.0)),
            min_inclusive: -1.0,
            max_exclusive: 1.0,
            when_in_range: Box::new(Density::Const(-0.0)),
            when_out_of_range: Box::new(Density::Const(1.0)),
        };
        let branch_program = Program::compile(&branch);
        let mut branch_scratch = Scratch::acquire(0, 4, 8, None);
        let mut branch_field = Field::new(
            branch_program.graph(),
            Geom {
                cell_width: 4,
                cell_height: 8,
            },
            &mut branch_scratch,
        );
        let branch_plan = branch_program
            .graph()
            .compile_tile_plan_for_test(branch_program.root());
        let values = branch_field.eval_tile_plan(&branch_plan, contexts, u8::MAX);
        assert_eq!(values[0].to_bits(), (-0.0_f64).to_bits());

        let interval = Density::IntervalSelect {
            input: Box::new(Density::YClampedGradient {
                from_y: -1.0,
                to_y: 1.0,
                from_value: -1.0,
                to_value: 1.0,
            }),
            thresholds: vec![0.0],
            functions: vec![
                Density::Const(-1.0),
                Density::Const(2.0),
                Density::Const(3.0),
            ],
        };
        let interval_program = Program::compile(&interval);
        let mut interval_scratch = Scratch::acquire(0, 4, 8, None);
        let mut interval_field = Field::new(
            interval_program.graph(),
            Geom {
                cell_width: 4,
                cell_height: 8,
            },
            &mut interval_scratch,
        );
        let interval_plan = interval_program
            .graph()
            .compile_tile_plan_for_test(interval_program.root());
        let actual = interval_field.eval_tile_plan(&interval_plan, contexts, u8::MAX);
        let mut scalar_scratch = Scratch::acquire(0, 4, 8, None);
        let mut scalar_field = Field::new(
            interval_program.graph(),
            Geom {
                cell_width: 4,
                cell_height: 8,
            },
            &mut scalar_scratch,
        );
        for (lane, (x, y, z)) in contexts.into_iter().enumerate() {
            assert_eq!(
                actual[lane].to_bits(),
                scalar_field.eval::<false>(interval_program.root(), x, y, z).to_bits(),
                "interval lane {lane}",
            );
        }
    }

    #[test]
    fn tile_eligibility_rejects_cache_writers_and_point_boundaries() {
        let cached = Density::FlatCache {
            inner: Box::new(Density::YClampedGradient {
                from_y: 0.0,
                to_y: 1.0,
                from_value: 0.0,
                to_value: 1.0,
            }),
            slot: 0,
            memo: crate::density::XzMemoId::NONE,
        };
        let program = Program::compile(&cached);
        assert!(!program.graph().tile_eligible(program.root()));
        let program = Program::compile(&Density::Spline(crate::density::Spline::Constant(1.0)));
        assert!(!program.graph().tile_eligible(program.root()));
    }

    fn eval_scalar(program: &Program, x: i32, y: i32, z: i32) -> f64 {
        let mut scratch = Scratch::acquire(1, 4, 8, None);
        let mut field = Field::new(
            program.graph(),
            Geom {
                cell_width: 4,
                cell_height: 8,
            },
            &mut scratch,
        );
        field.eval::<false>(program.root(), x, y, z)
    }

    fn varying() -> Density {
        Density::YClampedGradient {
            from_y: -1.0,
            to_y: 1.0,
            from_value: -0.5,
            to_value: 0.5,
        }
    }

    #[test]
    fn clamp_max_pure_left_returns_the_saturating_bound() {
        let root = Density::Clamp {
            input: Box::new(Density::Max(
                Box::new(varying()),
                Box::new(Density::Const(2.0)),
            )),
            min: -1.0,
            max: 1.0,
        };
        let program = Program::compile(&root);
        assert!(program
            .graph()
            .clamp_max_rhs_first(program.graph().op(program.root()))
            .is_some());
        assert_eq!(eval_scalar(&program, 0, 0, 0).to_bits(), 1.0_f64.to_bits());
    }

    #[test]
    fn clamp_max_nan_right_uses_the_normal_max_path() {
        let root = Density::Clamp {
            input: Box::new(Density::Max(
                Box::new(varying()),
                Box::new(Density::Const(f64::NAN)),
            )),
            min: -1.0,
            max: 1.0,
        };
        let program = Program::compile(&root);
        assert!(program
            .graph()
            .clamp_max_rhs_first(program.graph().op(program.root()))
            .is_some());
        assert_eq!(eval_scalar(&program, 0, -1, 0).to_bits(), (-0.5_f64).to_bits());
    }

    #[test]
    fn clamp_max_does_not_reorder_a_cache_writing_left() {
        let root = Density::Clamp {
            input: Box::new(Density::Max(
                Box::new(Density::FlatCache {
                    inner: Box::new(varying()),
                    slot: 0,
                    memo: crate::density::XzMemoId::NONE,
                }),
                Box::new(Density::Const(2.0)),
            )),
            min: -1.0,
            max: 1.0,
        };
        let program = Program::compile(&root);
        assert!(program
            .graph()
            .clamp_max_rhs_first(program.graph().op(program.root()))
            .is_none());
        assert_eq!(eval_scalar(&program, 0, 0, 0).to_bits(), 1.0_f64.to_bits());
    }

    #[test]
    fn interpolated_corner_simd_matches_scalar_for_all_lanes_and_masks() {
        let corner_sets = [
            [-3.25, 0.125, 7.5, -11.75, 19.0, -0.0, 2.5, -4.0],
            [f64::NAN, 0.125, 7.5, -11.75, 19.0, -0.0, 2.5, -4.0],
            [
                f64::from_bits(0xfff8_0000_0000_1234), 0.125, 7.5, -11.75,
                19.0, -0.0, 2.5, -4.0,
            ],
            [-0.0; 8],
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
                    let mut fallback = actual;
                    write_interpolated_corners(
                        4, 8, x0, y0, z0, mask, corners, &mut actual,
                    );
                    write_interpolated_corners_scalar(4, 8, mask, corners, &mut expected);
                    write_interpolated_corners_kernel(
                        fearless_simd::Fallback::new(), mask, corners, &mut fallback,
                    );
                    for (index, (&got, &want)) in actual.iter().zip(expected.iter()).enumerate() {
                        assert_eq!(
                            got.to_bits(),
                            want.to_bits(),
                            "corner lane {index}, origin ({x0},{y0},{z0}), mask {mask:?}",
                        );
                        assert_eq!(
                            fallback[index].to_bits(),
                            want.to_bits(),
                            "fallback corner lane {index}, origin ({x0},{y0},{z0}), mask {mask:?}",
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
            let mut fallback = actual;
            write_shared_noodle_output(4, 8, mask, &channels, &mut actual);
            write_shared_noodle_output_scalar(4, 8, mask, &channels, &mut expected);
            write_shared_noodle_output_kernel(
                fearless_simd::Fallback::new(), mask, &channels, &mut fallback,
            );
            for (index, (&got, &want)) in actual.iter().zip(expected.iter()).enumerate() {
                assert_eq!(
                    got.to_bits(),
                    want.to_bits(),
                    "noodle lane {index}, mask {mask:?}",
                );
                assert_eq!(
                    fallback[index].to_bits(),
                    want.to_bits(),
                    "fallback noodle lane {index}, mask {mask:?}",
                );
            }
        }
    }

    #[test]
    fn shared_noodle_output_has_exact_inactive_active_and_mixed_group_arithmetic() {
        let terrain = [
            -2.0, -0.0, 0.0, 0.375, 1.5, f64::NAN, f64::INFINITY, f64::NEG_INFINITY,
        ];
        let inactive: [f64; 8] = [
            -11.0 / 24.0, 0.0, 0.0, 759.0 / 4096.0,
            11.0 / 24.0, 64.0, 11.0 / 24.0, -11.0 / 24.0,
        ];
        // Constant corners give noodle = -5/16 + 3/2 * max(1/8, 1/4) = 1/16.
        let active: [f64; 8] = [
            -11.0 / 24.0, 0.0, 0.0, 1.0 / 16.0,
            1.0 / 16.0, 1.0 / 16.0, 1.0 / 16.0, -11.0 / 24.0,
        ];
        let channels = [[0.0; 8], [0.125; 8], [-0.25; 8], [-0.3125; 8]];
        let masks = [
            [0, 0],
            [u64::MAX, u64::MAX],
            [0x0000_00ff_0000_5500, 0xaa00_0000_ff00_0000],
        ];
        for mask in masks {
            let mut actual = std::array::from_fn(|index| terrain[index % 8]);
            let mut fallback = actual;
            write_shared_noodle_output(4, 8, mask, &channels, &mut actual);
            write_shared_noodle_output_kernel(
                fearless_simd::Fallback::new(), mask, &channels, &mut fallback,
            );
            for index in 0..128 {
                let expected = if (mask[index >> 6] >> (index & 63)) & 1 == 0 {
                    inactive[index % 8]
                } else {
                    active[index % 8]
                };
                assert_eq!(
                    actual[index].to_bits(), expected.to_bits(),
                    "noodle lane {index}, mask {mask:?}",
                );
                assert_eq!(
                    fallback[index].to_bits(), expected.to_bits(),
                    "fallback noodle lane {index}, mask {mask:?}",
                );
            }
        }

        let channels = [[f64::NAN; 8]; 4];
        let mut actual = std::array::from_fn(|index| terrain[index % 8]);
        let mut fallback = actual;
        write_shared_noodle_output(4, 8, [0, 0], &channels, &mut actual);
        write_shared_noodle_output_kernel(
            fearless_simd::Fallback::new(), [0, 0], &channels, &mut fallback,
        );
        for index in 0..128 {
            assert_eq!(
                actual[index].to_bits(), inactive[index % 8].to_bits(),
                "inactive NaN channel lane {index}",
            );
            assert_eq!(
                fallback[index].to_bits(), inactive[index % 8].to_bits(),
                "inactive NaN channel fallback lane {index}",
            );
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
    fn shared_noodle_cell_is_bitwise_at_negative_and_offgrid_origins_with_inactive_lanes() {
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
            &[(-8, -16, -8), (-7, -15, -7), (-3, -7, -3), (0, -8, 0), (0, 0, 0)],
        );
        compare(
            Density::YClampedGradient {
                from_y: -8.0,
                to_y: 8.0,
                from_value: -1.0,
                to_value: -1.0,
            },
            &[(-8, -16, -8), (-7, -15, -7), (-3, -7, -3), (0, 0, 0)],
        );
    }
}
