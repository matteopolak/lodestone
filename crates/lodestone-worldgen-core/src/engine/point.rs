//! A compiled point-semantics density evaluator.
//!
//! Compiled exact-point evaluation for density trees that contain
//! `find_top_surface`. The immutable graph is shareable; mutable xz memo state
//! lives in a bounded caller-owned [`PointScratch`].

use std::collections::HashMap;
use std::sync::Arc;

use crate::density::{Context, Density};
use crate::noise::NormalNoise;

type NodeId = u32;

/// The point evaluator's operator table. Values intentionally use the same
/// discriminants as [`Density::kind_index`] so diagnostics can compare the two
/// evaluators without a translation table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum PointKind {
    Const = 0,
    BlendAlpha = 1,
    BlendOffset = 2,
    Beardifier = 3,
    YClampedGradient = 4,
    Add = 5,
    Mul = 6,
    Min = 7,
    Max = 8,
    Abs = 9,
    Square = 10,
    Cube = 11,
    HalfNegative = 12,
    QuarterNegative = 13,
    Squeeze = 14,
    Invert = 15,
    Clamp = 16,
    Interpolated = 17,
    FlatCache = 18,
    Cache2D = 19,
    Marker = 20,
    Noise = 21,
    ShiftedNoise = 22,
    ShiftA = 23,
    ShiftB = 24,
    Shift = 25,
    RangeChoice = 26,
    IntervalSelect = 27,
    Spline = 28,
    Blended = 29,
    FindTopSurface = 30,
    EndIslands = 31,
}

impl PointKind {
    fn from_density(density: &Density) -> Self {
        match density {
            Density::Const(_) => Self::Const,
            Density::BlendAlpha => Self::BlendAlpha,
            Density::BlendOffset => Self::BlendOffset,
            Density::Beardifier => Self::Beardifier,
            Density::YClampedGradient { .. } => Self::YClampedGradient,
            Density::Add(_, _) => Self::Add,
            Density::Mul(_, _) => Self::Mul,
            Density::Min(_, _) => Self::Min,
            Density::Max(_, _) => Self::Max,
            Density::Abs(_) => Self::Abs,
            Density::Square(_) => Self::Square,
            Density::Cube(_) => Self::Cube,
            Density::HalfNegative(_) => Self::HalfNegative,
            Density::QuarterNegative(_) => Self::QuarterNegative,
            Density::Squeeze(_) => Self::Squeeze,
            Density::Invert(_) => Self::Invert,
            Density::Clamp { .. } => Self::Clamp,
            Density::Interpolated { .. } => Self::Interpolated,
            Density::FlatCache { .. } => Self::FlatCache,
            Density::Cache2D { .. } => Self::Cache2D,
            Density::Marker(_) => Self::Marker,
            Density::Noise { .. } => Self::Noise,
            Density::ShiftedNoise { .. } => Self::ShiftedNoise,
            Density::ShiftA(_) => Self::ShiftA,
            Density::ShiftB(_) => Self::ShiftB,
            Density::Shift(_) => Self::Shift,
            Density::RangeChoice { .. } => Self::RangeChoice,
            Density::IntervalSelect { .. } => Self::IntervalSelect,
            Density::Spline(_) => Self::Spline,
            Density::Blended(_) => Self::Blended,
            Density::FindTopSurface { .. } => Self::FindTopSurface,
            Density::EndIslands(_) => Self::EndIslands,
        }
    }
}

#[derive(Clone, Copy)]
struct PointOp {
    kind: PointKind,
    a: u32,
    b: u32,
    c: u32,
    d: u32,
}

#[derive(Clone, Copy)]
struct FindTopSurfaceInfo {
    lower_bound: i32,
    cell_height: i32,
}

struct PointGraph {
    ops: Vec<PointOp>,
    params: Vec<f64>,
    children: Vec<NodeId>,
    noises: Vec<NormalNoise>,
    leaves: Vec<Density>,
    find_top_surface: Vec<FindTopSurfaceInfo>,
    interner: HashMap<Vec<u64>, NodeId>,
    noise_interner: HashMap<Vec<u64>, u32>,
    leaf_interner: HashMap<Vec<u64>, u32>,
    shared_nodes: usize,
}

impl PointGraph {
    fn new() -> Self {
        Self {
            ops: Vec::new(),
            params: Vec::new(),
            children: Vec::new(),
            noises: Vec::new(),
            leaves: Vec::new(),
            find_top_surface: Vec::new(),
            interner: HashMap::new(),
            noise_interner: HashMap::new(),
            leaf_interner: HashMap::new(),
            shared_nodes: 0,
        }
    }

    fn compile(root: &Density) -> (Self, NodeId) {
        let mut graph = Self::new();
        let root = graph.compile_node(root);
        graph.interner.clear();
        graph.noise_interner.clear();
        graph.leaf_interner.clear();
        (graph, root)
    }

    fn compile_node(&mut self, density: &Density) -> NodeId {
        let kind = PointKind::from_density(density);
        match density {
            Density::Const(value) => self.intern(kind, &[*value], &[], None, None),
            Density::BlendAlpha | Density::BlendOffset | Density::Beardifier => {
                self.intern(kind, &[], &[], None, None)
            }
            Density::YClampedGradient {
                from_y,
                to_y,
                from_value,
                to_value,
            } => self.intern(
                kind,
                &[*from_y, *to_y, *from_value, *to_value],
                &[],
                None,
                None,
            ),
            Density::Add(left, right)
            | Density::Mul(left, right)
            | Density::Min(left, right)
            | Density::Max(left, right) => {
                let left = self.compile_node(left);
                let right = self.compile_node(right);
                self.intern(kind, &[], &[left, right], None, None)
            }
            Density::Abs(inner)
            | Density::Square(inner)
            | Density::Cube(inner)
            | Density::HalfNegative(inner)
            | Density::QuarterNegative(inner)
            | Density::Squeeze(inner)
            | Density::Invert(inner)
            | Density::Interpolated { inner, .. }
            | Density::Marker(inner) => {
                let inner = self.compile_node(inner);
                self.intern(kind, &[], &[inner], None, None)
            }
            Density::Clamp { input, min, max } => {
                let input = self.compile_node(input);
                self.intern(kind, &[*min, *max], &[input], None, None)
            }
            Density::FlatCache { inner, memo, .. } | Density::Cache2D { inner, memo } => {
                let inner_id = self.compile_node(inner);
                // Preserve the source builder's memo-presence decision.
                self.intern(kind, &[], &[inner_id], Some(u64::from(memo.is_some())), None)
            }
            Density::Noise {
                noise,
                xz_scale,
                y_scale,
            } => {
                let noise = self.push_noise(noise);
                self.intern(kind, &[*xz_scale, *y_scale], &[], None, Some(noise))
            }
            Density::ShiftedNoise {
                shift_x,
                shift_y,
                shift_z,
                xz_scale,
                y_scale,
                noise,
            } => {
                let shift_x = self.compile_node(shift_x);
                let shift_y = self.compile_node(shift_y);
                let shift_z = self.compile_node(shift_z);
                let noise = self.push_noise(noise);
                self.intern(
                    kind,
                    &[*xz_scale, *y_scale],
                    &[shift_x, shift_y, shift_z],
                    None,
                    Some(noise),
                )
            }
            Density::ShiftA(noise) | Density::ShiftB(noise) | Density::Shift(noise) => {
                let noise = self.push_noise(noise);
                self.intern(kind, &[], &[], None, Some(noise))
            }
            Density::RangeChoice {
                input,
                min_inclusive,
                max_exclusive,
                when_in_range,
                when_out_of_range,
            } => {
                let input = self.compile_node(input);
                let when_in_range = self.compile_node(when_in_range);
                let when_out_of_range = self.compile_node(when_out_of_range);
                self.intern(
                    kind,
                    &[*min_inclusive, *max_exclusive],
                    &[input, when_in_range, when_out_of_range],
                    None,
                    None,
                )
            }
            Density::IntervalSelect {
                input,
                thresholds,
                functions,
            } => {
                let input = self.compile_node(input);
                let functions: Vec<_> = functions
                    .iter()
                    .map(|function| self.compile_node(function))
                    .collect();
                let mut children = Vec::with_capacity(functions.len() + 1);
                children.push(input);
                children.extend(functions);
                self.intern(kind, thresholds, &children, None, None)
            }
            Density::Spline(_) | Density::Blended(_) | Density::EndIslands(_) => {
                let leaf = self.push_leaf(density);
                self.intern(kind, &[], &[], None, Some(leaf))
            }
            Density::FindTopSurface {
                density,
                upper_bound,
                lower_bound,
                cell_height,
            } => {
                let density = self.compile_node(density);
                let upper_bound = self.compile_node(upper_bound);
                let info = self.find_top_surface.len() as u32;
                self.find_top_surface.push(FindTopSurfaceInfo {
                    lower_bound: *lower_bound,
                    cell_height: *cell_height,
                });
                let bounds = (u64::from(*lower_bound as u32) << 32)
                    | u64::from(*cell_height as u32);
                self.intern(kind, &[], &[density, upper_bound], Some(bounds), Some(info))
            }
        }
    }

    fn push_noise(&mut self, noise: &NormalNoise) -> u32 {
        let mut signature = Vec::new();
        noise.write_signature(&mut signature);
        if let Some(&index) = self.noise_interner.get(&signature) {
            return index;
        }
        let index = self.noises.len() as u32;
        self.noises.push(noise.clone());
        self.noise_interner.insert(signature, index);
        index
    }

    fn push_leaf(&mut self, density: &Density) -> u32 {
        let mut signature = Vec::new();
        density.write_signature(&mut signature);
        if let Some(&index) = self.leaf_interner.get(&signature) {
            return index;
        }
        let index = self.leaves.len() as u32;
        self.leaves.push(density.clone());
        self.leaf_interner.insert(signature, index);
        index
    }

    fn intern(
        &mut self,
        kind: PointKind,
        params: &[f64],
        children: &[NodeId],
        flag: Option<u64>,
        side_index: Option<u32>,
    ) -> NodeId {
        let mut key = Vec::with_capacity(3 + params.len() + children.len());
        key.push(u64::from(kind as u8));
        key.push(flag.unwrap_or(u64::MAX));
        let key_side_index = if kind == PointKind::FindTopSurface {
            None
        } else {
            side_index
        };
        key.push(key_side_index.map(u64::from).unwrap_or(u64::MAX));
        key.push(children.len() as u64);
        key.extend(children.iter().map(|child| u64::from(*child)));
        key.push(params.len() as u64);
        key.extend(params.iter().map(|value| value.to_bits()));
        if let Some(&id) = self.interner.get(&key) {
            self.shared_nodes += 1;
            return id;
        }

        let params_at = self.params.len() as u32;
        self.params.extend_from_slice(params);
        let children_at = self.children.len() as u32;
        self.children.extend_from_slice(children);

        let op = match kind {
            PointKind::Const | PointKind::YClampedGradient | PointKind::Clamp => PointOp {
                kind,
                a: params_at,
                b: if kind == PointKind::Clamp {
                    children[0]
                } else {
                    0
                },
                c: 0,
                d: 0,
            },
            PointKind::Noise => PointOp {
                kind,
                a: side_index.expect("noise payload"),
                b: params_at,
                c: 0,
                d: 0,
            },
            PointKind::ShiftedNoise => PointOp {
                kind,
                a: children_at,
                b: side_index.expect("noise payload"),
                c: params_at,
                d: 0,
            },
            PointKind::RangeChoice => PointOp {
                kind,
                a: children_at,
                b: params_at,
                c: 0,
                d: 0,
            },
            PointKind::IntervalSelect => PointOp {
                kind,
                a: children_at,
                b: params_at,
                c: params.len() as u32,
                d: (children.len() - 1) as u32,
            },
            PointKind::FindTopSurface => PointOp {
                kind,
                a: children[0],
                b: children[1],
                c: side_index.expect("find-top metadata"),
                d: 0,
            },
            PointKind::FlatCache | PointKind::Cache2D => PointOp {
                kind,
                a: children[0],
                b: flag.unwrap_or(0) as u32,
                c: 0,
                d: 0,
            },
            PointKind::Spline | PointKind::Blended | PointKind::EndIslands => PointOp {
                kind,
                a: side_index.expect("leaf payload"),
                b: 0,
                c: 0,
                d: 0,
            },
            _ if kind == PointKind::Add
                || kind == PointKind::Mul
                || kind == PointKind::Min
                || kind == PointKind::Max => PointOp {
                kind,
                a: children[0],
                b: children[1],
                c: 0,
                d: 0,
            },
            _ => PointOp {
                kind,
                a: children.first().copied().unwrap_or(0),
                b: 0,
                c: 0,
                d: 0,
            },
        };
        let id = self.ops.len() as u32;
        self.ops.push(op);
        self.interner.insert(key, id);
        id
    }
}

/// Request-local state for [`PointProgram`] evaluation.
#[derive(Debug)]
pub struct PointScratch {
    entries: Vec<PointMemoEntry>,
    /// Lazily sized per-node values for one fixed-width batch.
    batch_values: Vec<f64>,
    #[cfg(feature = "gen-counters")]
    hits: u64,
    #[cfg(feature = "gen-counters")]
    misses: u64,
}

#[derive(Clone, Copy, Debug)]
struct PointMemoEntry {
    node: NodeId,
    x: i32,
    z: i32,
    value: f64,
}

/// Fixed lane count; batch scratch remains proportional to graph size.
const POINT_BATCH_WIDTH: usize = 8;

const POINT_MEMO_EMPTY: PointMemoEntry = PointMemoEntry {
    node: u32::MAX,
    x: 0,
    z: 0,
    value: 0.0,
};

impl PointScratch {
    /// Creates a request-local memo with at most `capacity` entries.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            entries: vec![POINT_MEMO_EMPTY; capacity],
            batch_values: Vec::new(),
            #[cfg(feature = "gen-counters")]
            hits: 0,
            #[cfg(feature = "gen-counters")]
            misses: 0,
        }
    }

    /// Creates the default bounded memo.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(4096)
    }

    #[inline]
    fn index(&self, node: NodeId, x: i32, z: i32) -> usize {
        let mut hash = u64::from(node).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        hash ^= u64::from(x as u32) << 32;
        hash ^= u64::from(z as u32);
        // Fold X into the low bits before the avalanche used for indexing.
        hash ^= hash >> 32;
        hash = (hash ^ (hash >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        hash = (hash ^ (hash >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        hash ^= hash >> 31;
        (hash as usize) % self.entries.len()
    }

    #[inline]
    fn get(&mut self, node: NodeId, x: i32, z: i32) -> Option<f64> {
        let entry = self.entries[self.index(node, x, z)];
        let hit = entry.node == node && entry.x == x && entry.z == z;
        #[cfg(feature = "gen-counters")]
        if hit {
            self.hits += 1;
        } else {
            self.misses += 1;
        }
        hit.then_some(entry.value)
    }

    #[inline]
    fn put(&mut self, node: NodeId, x: i32, z: i32, value: f64) {
        let index = self.index(node, x, z);
        self.entries[index] = PointMemoEntry { node, x, z, value };
    }

    /// Returns request-local memo hit/miss counts when diagnostics are enabled.
    #[must_use]
    pub fn memo_stats(&self) -> (u64, u64) {
        #[cfg(feature = "gen-counters")]
        {
            (self.hits, self.misses)
        }
        #[cfg(not(feature = "gen-counters"))]
        {
            (0, 0)
        }
    }

    /// Bytes currently reserved for the batch value table.
    #[must_use]
    pub fn batch_buffer_bytes(&self) -> usize {
        self.batch_values.len() * std::mem::size_of::<f64>()
    }

    #[inline]
    fn prepare_batch(&mut self, node_count: usize) {
        let required = node_count * POINT_BATCH_WIDTH;
        if self.batch_values.len() < required {
            self.batch_values.resize(required, 0.0);
        }
    }

    #[inline]
    fn batch_value(&self, node: NodeId, lane: usize) -> f64 {
        self.batch_values[node as usize * POINT_BATCH_WIDTH + lane]
    }

    #[inline]
    fn set_batch_value(&mut self, node: NodeId, lane: usize, value: f64) {
        self.batch_values[node as usize * POINT_BATCH_WIDTH + lane] = value;
    }
}

impl Default for PointScratch {
    fn default() -> Self {
        Self::new()
    }
}

/// A compiled point-semantics density tree.
#[derive(Clone)]
pub struct PointProgram {
    graph: Arc<PointGraph>,
    root: NodeId,
}

impl std::fmt::Debug for PointProgram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PointProgram")
            .field("node_count", &self.node_count())
            .field("root", &self.root)
            .finish()
    }
}

impl PointProgram {
    /// Compiles a density tree for exact point evaluation.
    #[must_use]
    pub fn compile(root: &Density) -> Self {
        let (graph, root) = PointGraph::compile(root);
        Self {
            graph: Arc::new(graph),
            root,
        }
    }

    /// Number of emitted point operations after structural sharing.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.graph.ops.len()
    }

    /// Number of duplicate source nodes answered by the compiler.
    #[must_use]
    pub fn shared_nodes(&self) -> usize {
        self.graph.shared_nodes
    }

    /// Number of xz-cache wrappers in the flattened point graph.
    #[must_use]
    pub fn cache_node_count(&self) -> usize {
        self.graph
            .ops
            .iter()
            .filter(|op| matches!(op.kind, PointKind::FlatCache | PointKind::Cache2D))
            .count()
    }

    /// Whether the root operation is a `find_top_surface` scan.
    #[must_use]
    pub fn is_find_top_surface(&self) -> bool {
        self.graph.ops[self.root as usize].kind == PointKind::FindTopSurface
    }

    /// Evaluates one context with exact point semantics.
    #[must_use]
    pub fn compute(&self, ctx: Context, scratch: &mut PointScratch) -> f64 {
        self.eval(self.root, ctx, scratch)
    }

    /// Evaluates a bounded batch of contexts into `output`.
    ///
    /// Contexts are visited in input order in fixed groups of eight. Branch and
    /// scan order remain exactly those of point evaluation, and xz-pure cache
    /// entries can be reused by later contexts in the same request.
    pub fn compute_batch(
        &self,
        contexts: &[Context],
        output: &mut [f64],
        scratch: &mut PointScratch,
    ) {
        assert_eq!(contexts.len(), output.len());
        scratch.prepare_batch(self.graph.ops.len());
        for (offset, batch) in contexts.chunks(POINT_BATCH_WIDTH).enumerate() {
            let lanes = batch.len();
            let mask = if lanes == POINT_BATCH_WIDTH {
                u8::MAX
            } else {
                (1u8 << lanes) - 1
            };
            self.eval_batch(self.root, batch, mask, scratch);
            for lane in 0..lanes {
                output[offset * POINT_BATCH_WIDTH + lane] =
                    scratch.batch_value(self.root, lane);
            }
        }
    }

    fn eval_batch(
        &self,
        id: NodeId,
        contexts: &[Context],
        mask: u8,
        scratch: &mut PointScratch,
    ) {
        if mask == 0 {
            return;
        }
        let op = self.graph.ops[id as usize];
        if !matches!(op.kind, PointKind::Spline | PointKind::Blended | PointKind::EndIslands) {
            for lane in 0..contexts.len() {
                if mask & (1 << lane) != 0 {
                    crate::counters::bump_density_point_compute(op.kind as usize);
                    crate::engine::redundancy_probe::visit_point(
                        std::ptr::from_ref(self.graph.as_ref()).cast::<()>(),
                        op.kind as usize,
                        contexts[lane].x,
                        contexts[lane].y,
                        contexts[lane].z,
                    );
                }
            }
        }

        match op.kind {
            PointKind::Const => {
                let value = self.graph.params[op.a as usize];
                for lane in 0..contexts.len() {
                    if mask & (1 << lane) != 0 {
                        scratch.set_batch_value(id, lane, value);
                    }
                }
            }
            PointKind::BlendAlpha => {
                for lane in 0..contexts.len() {
                    if mask & (1 << lane) != 0 {
                        scratch.set_batch_value(id, lane, 1.0);
                    }
                }
            }
            PointKind::BlendOffset | PointKind::Beardifier => {
                for lane in 0..contexts.len() {
                    if mask & (1 << lane) != 0 {
                        scratch.set_batch_value(id, lane, 0.0);
                    }
                }
            }
            PointKind::YClampedGradient => {
                let from_y = self.graph.params[op.a as usize];
                let to_y = self.graph.params[(op.a + 1) as usize];
                let from_value = self.graph.params[(op.a + 2) as usize];
                let to_value = self.graph.params[(op.a + 3) as usize];
                for lane in 0..contexts.len() {
                    if mask & (1 << lane) != 0 {
                        scratch.set_batch_value(
                            id,
                            lane,
                            crate::math::clamped_map(
                                f64::from(contexts[lane].y),
                                from_y,
                                to_y,
                                from_value,
                                to_value,
                            ),
                        );
                    }
                }
            }
            PointKind::Add | PointKind::Min | PointKind::Max => {
                self.eval_batch(op.a, contexts, mask, scratch);
                self.eval_batch(op.b, contexts, mask, scratch);
                for lane in 0..contexts.len() {
                    if mask & (1 << lane) != 0 {
                        let left = scratch.batch_value(op.a, lane);
                        let right = scratch.batch_value(op.b, lane);
                        let value = match op.kind {
                            PointKind::Add => left + right,
                            PointKind::Min => left.min(right),
                            PointKind::Max => left.max(right),
                            _ => unreachable!(),
                        };
                        scratch.set_batch_value(id, lane, value);
                    }
                }
            }
            PointKind::Mul => {
                self.eval_batch(op.a, contexts, mask, scratch);
                let mut right_mask = 0;
                for lane in 0..contexts.len() {
                    if mask & (1 << lane) != 0 && scratch.batch_value(op.a, lane) != 0.0 {
                        right_mask |= 1 << lane;
                    }
                }
                self.eval_batch(op.b, contexts, right_mask, scratch);
                for lane in 0..contexts.len() {
                    if mask & (1 << lane) != 0 {
                        let left = scratch.batch_value(op.a, lane);
                        let value = if left == 0.0 {
                            0.0
                        } else {
                            left * scratch.batch_value(op.b, lane)
                        };
                        scratch.set_batch_value(id, lane, value);
                    }
                }
            }
            PointKind::Abs
            | PointKind::Square
            | PointKind::Cube
            | PointKind::HalfNegative
            | PointKind::QuarterNegative
            | PointKind::Squeeze
            | PointKind::Invert => {
                self.eval_batch(op.a, contexts, mask, scratch);
                for lane in 0..contexts.len() {
                    if mask & (1 << lane) == 0 {
                        continue;
                    }
                    let value = scratch.batch_value(op.a, lane);
                    let value = match op.kind {
                        PointKind::Abs => value.abs(),
                        PointKind::Square => value * value,
                        PointKind::Cube => value * value * value,
                        PointKind::HalfNegative => {
                            if value > 0.0 { value } else { value * 0.5 }
                        }
                        PointKind::QuarterNegative => {
                            if value > 0.0 { value } else { value * 0.25 }
                        }
                        PointKind::Squeeze => {
                            let value = crate::math::clamp(value, -1.0, 1.0);
                            value / 2.0 - value * value * value / 24.0
                        }
                        PointKind::Invert => 1.0 / value,
                        _ => unreachable!(),
                    };
                    scratch.set_batch_value(id, lane, value);
                }
            }
            PointKind::Clamp => {
                self.eval_batch(op.b, contexts, mask, scratch);
                let min = self.graph.params[op.a as usize];
                let max = self.graph.params[(op.a + 1) as usize];
                for lane in 0..contexts.len() {
                    if mask & (1 << lane) != 0 {
                        scratch.set_batch_value(
                            id,
                            lane,
                            crate::math::clamp(scratch.batch_value(op.b, lane), min, max),
                        );
                    }
                }
            }
            PointKind::Interpolated | PointKind::Marker => {
                self.eval_batch(op.a, contexts, mask, scratch);
                for lane in 0..contexts.len() {
                    if mask & (1 << lane) != 0 {
                        scratch.set_batch_value(id, lane, scratch.batch_value(op.a, lane));
                    }
                }
            }
            PointKind::FlatCache | PointKind::Cache2D => {
                if op.b == 0 {
                    self.eval_batch(op.a, contexts, mask, scratch);
                    for lane in 0..contexts.len() {
                        if mask & (1 << lane) != 0 {
                            scratch.set_batch_value(id, lane, scratch.batch_value(op.a, lane));
                        }
                    }
                } else {
                    let mut misses = 0;
                    for lane in 0..contexts.len() {
                        if mask & (1 << lane) == 0 {
                            continue;
                        }
                        let ctx = contexts[lane];
                        if let Some(value) = scratch.get(id, ctx.x, ctx.z) {
                            scratch.set_batch_value(id, lane, value);
                        } else {
                            misses |= 1 << lane;
                        }
                    }
                    self.eval_batch(op.a, contexts, misses, scratch);
                    for lane in 0..contexts.len() {
                        if misses & (1 << lane) != 0 {
                            let value = scratch.batch_value(op.a, lane);
                            let ctx = contexts[lane];
                            scratch.put(id, ctx.x, ctx.z, value);
                            scratch.set_batch_value(id, lane, value);
                        }
                    }
                }
            }
            PointKind::Noise => {
                let noise = &self.graph.noises[op.a as usize];
                let xz = self.graph.params[op.b as usize];
                let y_scale = self.graph.params[(op.b + 1) as usize];
                for lane in 0..contexts.len() {
                    if mask & (1 << lane) != 0 {
                        let ctx = contexts[lane];
                        scratch.set_batch_value(
                            id,
                            lane,
                            noise.get_value(
                                f64::from(ctx.x) * xz,
                                f64::from(ctx.y) * y_scale,
                                f64::from(ctx.z) * xz,
                            ),
                        );
                    }
                }
            }
            PointKind::ShiftedNoise => {
                let shift_x = self.graph.children[op.a as usize];
                let shift_y = self.graph.children[(op.a + 1) as usize];
                let shift_z = self.graph.children[(op.a + 2) as usize];
                self.eval_batch(shift_x, contexts, mask, scratch);
                self.eval_batch(shift_y, contexts, mask, scratch);
                self.eval_batch(shift_z, contexts, mask, scratch);
                let xz = self.graph.params[op.c as usize];
                let y_scale = self.graph.params[(op.c + 1) as usize];
                let noise = &self.graph.noises[op.b as usize];
                for lane in 0..contexts.len() {
                    if mask & (1 << lane) != 0 {
                        let ctx = contexts[lane];
                        let x = f64::from(ctx.x) * xz + scratch.batch_value(shift_x, lane);
                        let y = f64::from(ctx.y) * y_scale + scratch.batch_value(shift_y, lane);
                        let z = f64::from(ctx.z) * xz + scratch.batch_value(shift_z, lane);
                        scratch.set_batch_value(id, lane, noise.get_value(x, y, z));
                    }
                }
            }
            PointKind::ShiftA | PointKind::ShiftB | PointKind::Shift => {
                let noise = &self.graph.noises[op.a as usize];
                for lane in 0..contexts.len() {
                    if mask & (1 << lane) != 0 {
                        let ctx = contexts[lane];
                        let value = match op.kind {
                            PointKind::ShiftA => {
                                shift(noise, f64::from(ctx.x), 0.0, f64::from(ctx.z))
                            }
                            PointKind::ShiftB => {
                                shift(noise, f64::from(ctx.z), f64::from(ctx.x), 0.0)
                            }
                            PointKind::Shift => {
                                shift(noise, f64::from(ctx.x), f64::from(ctx.y), f64::from(ctx.z))
                            }
                            _ => unreachable!(),
                        };
                        scratch.set_batch_value(id, lane, value);
                    }
                }
            }
            PointKind::RangeChoice => {
                let input = self.graph.children[op.a as usize];
                self.eval_batch(input, contexts, mask, scratch);
                let min = self.graph.params[op.b as usize];
                let max = self.graph.params[(op.b + 1) as usize];
                let mut in_range = 0;
                let mut out_of_range = 0;
                for lane in 0..contexts.len() {
                    if mask & (1 << lane) == 0 {
                        continue;
                    }
                    let value = scratch.batch_value(input, lane);
                    if value >= min && value < max {
                        in_range |= 1 << lane;
                    } else {
                        out_of_range |= 1 << lane;
                    }
                }
                let when_in_range = self.graph.children[(op.a + 1) as usize];
                let when_out_of_range = self.graph.children[(op.a + 2) as usize];
                self.eval_batch(when_in_range, contexts, in_range, scratch);
                self.eval_batch(when_out_of_range, contexts, out_of_range, scratch);
                for lane in 0..contexts.len() {
                    if mask & (1 << lane) != 0 {
                        let child = if in_range & (1 << lane) != 0 {
                            when_in_range
                        } else {
                            when_out_of_range
                        };
                        scratch.set_batch_value(id, lane, scratch.batch_value(child, lane));
                    }
                }
            }
            PointKind::IntervalSelect => {
                let input = self.graph.children[op.a as usize];
                self.eval_batch(input, contexts, mask, scratch);
                let mut selected = [0usize; POINT_BATCH_WIDTH];
                for lane in 0..contexts.len() {
                    if mask & (1 << lane) == 0 {
                        continue;
                    }
                    let value = scratch.batch_value(input, lane);
                    let mut index = op.c as usize;
                    for threshold in 0..op.c as usize {
                        if value < self.graph.params[op.b as usize + threshold] {
                            index = threshold;
                            break;
                        }
                    }
                    selected[lane] = index;
                }
                for index in 0..op.d as usize {
                    let mut branch_mask = 0;
                    for lane in 0..contexts.len() {
                        if mask & (1 << lane) != 0 && selected[lane] == index {
                            branch_mask |= 1 << lane;
                        }
                    }
                    if branch_mask != 0 {
                        let child = self.graph.children[op.a as usize + 1 + index];
                        self.eval_batch(child, contexts, branch_mask, scratch);
                    }
                }
                for lane in 0..contexts.len() {
                    if mask & (1 << lane) != 0 {
                        let child = self.graph.children[op.a as usize + 1 + selected[lane]];
                        scratch.set_batch_value(id, lane, scratch.batch_value(child, lane));
                    }
                }
            }
            PointKind::Spline | PointKind::Blended | PointKind::EndIslands => {
                let leaf = &self.graph.leaves[op.a as usize];
                for lane in 0..contexts.len() {
                    if mask & (1 << lane) != 0 {
                        scratch.set_batch_value(id, lane, leaf.compute(contexts[lane]));
                    }
                }
            }
            PointKind::FindTopSurface => {
                self.eval_batch(op.b, contexts, mask, scratch);
                let info = self.graph.find_top_surface[op.c as usize];
                let lower = info.lower_bound;
                let step = info.cell_height;
                let mut scan_y = [lower; POINT_BATCH_WIDTH];
                let mut scan_contexts = [Context::new(0, 0, 0); POINT_BATCH_WIDTH];
                let mut active = 0;
                for lane in 0..contexts.len() {
                    if mask & (1 << lane) == 0 {
                        continue;
                    }
                    let top_y =
                        (scratch.batch_value(op.b, lane) / f64::from(step)).floor() as i32 * step;
                    scratch.set_batch_value(id, lane, f64::from(lower));
                    if top_y > lower {
                        scan_y[lane] = top_y;
                        active |= 1 << lane;
                    }
                }
                while active != 0 {
                    for lane in 0..contexts.len() {
                        if active & (1 << lane) != 0 {
                            let ctx = contexts[lane];
                            scan_contexts[lane] = Context::new(ctx.x, scan_y[lane], ctx.z);
                        }
                    }
                    self.eval_batch(op.a, &scan_contexts[..contexts.len()], active, scratch);
                    for lane in 0..contexts.len() {
                        if active & (1 << lane) == 0 {
                            continue;
                        }
                        if scratch.batch_value(op.a, lane) > 0.0 {
                            scratch.set_batch_value(id, lane, f64::from(scan_y[lane]));
                            active &= !(1 << lane);
                        } else {
                            scan_y[lane] -= step;
                            if scan_y[lane] < lower {
                                active &= !(1 << lane);
                            }
                        }
                    }
                }
            }
        }
    }

    fn eval(&self, id: NodeId, ctx: Context, scratch: &mut PointScratch) -> f64 {
        let op = self.graph.ops[id as usize];
        if !matches!(op.kind, PointKind::Spline | PointKind::Blended | PointKind::EndIslands) {
            crate::counters::bump_density_point_compute(op.kind as usize);
            crate::engine::redundancy_probe::visit_point(
                std::ptr::from_ref(self.graph.as_ref()).cast::<()>(),
                op.kind as usize,
                ctx.x,
                ctx.y,
                ctx.z,
            );
        }
        match op.kind {
            PointKind::Const => self.graph.params[op.a as usize],
            PointKind::BlendAlpha => 1.0,
            PointKind::BlendOffset | PointKind::Beardifier => 0.0,
            PointKind::YClampedGradient => crate::math::clamped_map(
                f64::from(ctx.y),
                self.graph.params[op.a as usize],
                self.graph.params[(op.a + 1) as usize],
                self.graph.params[(op.a + 2) as usize],
                self.graph.params[(op.a + 3) as usize],
            ),
            PointKind::Add => {
                self.eval(op.a, ctx, scratch) + self.eval(op.b, ctx, scratch)
            }
            PointKind::Mul => {
                let first = self.eval(op.a, ctx, scratch);
                if first == 0.0 {
                    0.0
                } else {
                    first * self.eval(op.b, ctx, scratch)
                }
            }
            PointKind::Min => self
                .eval(op.a, ctx, scratch)
                .min(self.eval(op.b, ctx, scratch)),
            PointKind::Max => self
                .eval(op.a, ctx, scratch)
                .max(self.eval(op.b, ctx, scratch)),
            PointKind::Abs => self.eval(op.a, ctx, scratch).abs(),
            PointKind::Square => {
                let value = self.eval(op.a, ctx, scratch);
                value * value
            }
            PointKind::Cube => {
                let value = self.eval(op.a, ctx, scratch);
                value * value * value
            }
            PointKind::HalfNegative => {
                let value = self.eval(op.a, ctx, scratch);
                if value > 0.0 { value } else { value * 0.5 }
            }
            PointKind::QuarterNegative => {
                let value = self.eval(op.a, ctx, scratch);
                if value > 0.0 { value } else { value * 0.25 }
            }
            PointKind::Squeeze => {
                let value = crate::math::clamp(self.eval(op.a, ctx, scratch), -1.0, 1.0);
                value / 2.0 - value * value * value / 24.0
            }
            PointKind::Invert => 1.0 / self.eval(op.a, ctx, scratch),
            PointKind::Clamp => crate::math::clamp(
                self.eval(op.b, ctx, scratch),
                self.graph.params[op.a as usize],
                self.graph.params[(op.a + 1) as usize],
            ),
            PointKind::Interpolated | PointKind::Marker => self.eval(op.a, ctx, scratch),
            PointKind::FlatCache | PointKind::Cache2D => {
                if op.b == 0 {
                    return self.eval(op.a, ctx, scratch);
                }
                if let Some(value) = scratch.get(id, ctx.x, ctx.z) {
                    return value;
                }
                let value = self.eval(op.a, ctx, scratch);
                scratch.put(id, ctx.x, ctx.z, value);
                value
            }
            PointKind::Noise => {
                let noise = &self.graph.noises[op.a as usize];
                let xz = self.graph.params[op.b as usize];
                let y = self.graph.params[(op.b + 1) as usize];
                noise.get_value(
                    f64::from(ctx.x) * xz,
                    f64::from(ctx.y) * y,
                    f64::from(ctx.z) * xz,
                )
            }
            PointKind::ShiftedNoise => {
                let xz = self.graph.params[op.c as usize];
                let y_scale = self.graph.params[(op.c + 1) as usize];
                let x = f64::from(ctx.x) * xz
                    + self.eval(self.graph.children[op.a as usize], ctx, scratch);
                let y = f64::from(ctx.y) * y_scale
                    + self.eval(self.graph.children[(op.a + 1) as usize], ctx, scratch);
                let z = f64::from(ctx.z) * xz
                    + self.eval(self.graph.children[(op.a + 2) as usize], ctx, scratch);
                self.graph.noises[op.b as usize].get_value(x, y, z)
            }
            PointKind::ShiftA => shift(&self.graph.noises[op.a as usize], f64::from(ctx.x), 0.0, f64::from(ctx.z)),
            PointKind::ShiftB => shift(&self.graph.noises[op.a as usize], f64::from(ctx.z), f64::from(ctx.x), 0.0),
            PointKind::Shift => shift(&self.graph.noises[op.a as usize], f64::from(ctx.x), f64::from(ctx.y), f64::from(ctx.z)),
            PointKind::RangeChoice => {
                let input = self.eval(self.graph.children[op.a as usize], ctx, scratch);
                let child = if input >= self.graph.params[op.b as usize]
                    && input < self.graph.params[(op.b + 1) as usize]
                {
                    self.graph.children[(op.a + 1) as usize]
                } else {
                    self.graph.children[(op.a + 2) as usize]
                };
                self.eval(child, ctx, scratch)
            }
            PointKind::IntervalSelect => {
                let input = self.eval(self.graph.children[op.a as usize], ctx, scratch);
                let selected = (0..op.c as usize).find(|&index| {
                    input < self.graph.params[(op.b as usize) + index]
                });
                let selected = selected.unwrap_or_else(|| {
                    (op.d as usize).checked_sub(1).expect("interval has no functions")
                });
                let child = self.graph.children[op.a as usize + 1 + selected];
                self.eval(child, ctx, scratch)
            }
            PointKind::Spline | PointKind::Blended | PointKind::EndIslands => {
                self.graph.leaves[op.a as usize].compute(ctx)
            }
            PointKind::FindTopSurface => {
                let upper_bound = self.eval(op.b, ctx, scratch);
                let info = self.graph.find_top_surface[op.c as usize];
                let lower = info.lower_bound;
                let step = info.cell_height;
                let top_y = (upper_bound / f64::from(step)).floor() as i32 * step;
                if top_y <= lower {
                    return f64::from(lower);
                }
                let mut block_y = top_y;
                while block_y >= lower {
                    if self.eval(op.a, Context::new(ctx.x, block_y, ctx.z), scratch) > 0.0 {
                        return f64::from(block_y);
                    }
                    block_y -= step;
                }
                f64::from(lower)
            }
        }
    }
}

fn shift(noise: &NormalNoise, x: f64, y: f64, z: f64) -> f64 {
    noise.get_value(x * 0.25, y * 0.25, z * 0.25) * 4.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(density: Density) -> Box<Density> {
        Box::new(density)
    }

    fn fixture() -> Density {
        let offset = Density::Cache2D {
            inner: b(Density::YClampedGradient {
                from_y: 0.0,
                to_y: 1.0,
                from_value: -1.0,
                to_value: 1.0,
            }),
            memo: crate::density::XzMemoId::NONE,
        };
        let xz = Density::Cache2D {
            inner: b(Density::Const(0.5)),
            memo: crate::density::XzMemoId::NONE,
        };
        Density::FindTopSurface {
            density: b(Density::Add(
                b(Density::YClampedGradient {
                    from_y: -16.0,
                    to_y: 16.0,
                    from_value: -1.0,
                    to_value: 1.0,
                }),
                b(Density::Add(b(offset), b(xz))),
            )),
            upper_bound: b(Density::Const(32.0)),
            lower_bound: -16,
            cell_height: 8,
        }
    }

    #[test]
    fn compiled_find_top_surface_matches_point_bits() {
        let density = fixture();
        let program = PointProgram::compile(&density);
        assert!(program.is_find_top_surface());
        assert_eq!(program.cache_node_count(), 2);
        let mut scratch = PointScratch::with_capacity(8);
        for x in -3..=3 {
            for y in [-17, -16, 0, 17, 63] {
                for z in -3..=3 {
                    let context = Context::new(x, y, z);
                    assert_eq!(
                        program.compute(context, &mut scratch).to_bits(),
                        density.compute(context).to_bits(),
                        "point mismatch at ({x}, {y}, {z})"
                    );
                }
            }
        }
    }

    #[test]
    fn batch_preserves_input_order_and_bits() {
        let density = fixture();
        let program = PointProgram::compile(&density);
        let contexts: Vec<_> = (-9..=9)
            .map(|x| Context::new(x, if x % 2 == 0 { -16 } else { 17 }, x * 3))
            .collect();
        let mut output = vec![0.0; contexts.len()];
        let mut scratch = PointScratch::with_capacity(2);
        program.compute_batch(&contexts, &mut output, &mut scratch);
        for (context, value) in contexts.iter().copied().zip(output) {
            assert_eq!(value.to_bits(), density.compute(context).to_bits());
        }
    }

    #[test]
    fn duplicate_point_subtrees_are_shared_but_non_xz_cache_is_not_used() {
        let gradient = || Density::YClampedGradient {
            from_y: 0.0,
            to_y: 8.0,
            from_value: -1.0,
            to_value: 1.0,
        };
        let density = Density::FindTopSurface {
            density: b(Density::Add(
                b(Density::Cache2D {
                    inner: b(gradient()),
                    memo: crate::density::XzMemoId::NONE,
                }),
                b(Density::Cache2D {
                    inner: b(gradient()),
                    memo: crate::density::XzMemoId::NONE,
                }),
            )),
            upper_bound: b(Density::Const(8.0)),
            lower_bound: 0,
            cell_height: 8,
        };
        let program = PointProgram::compile(&density);
        assert!(program.shared_nodes() >= 2);
        let mut scratch = PointScratch::with_capacity(4);
        assert_eq!(
            program.compute(Context::new(1, 0, 2), &mut scratch).to_bits(),
            density.compute(Context::new(1, 0, 2)).to_bits()
        );
    }

    #[test]
    fn batch_interval_select_supports_more_branches_than_lanes() {
        let functions = (-4..=4).map(|value| Density::Const(f64::from(value))).collect();
        let density = Density::FindTopSurface {
            density: b(Density::IntervalSelect {
                input: b(Density::YClampedGradient {
                    from_y: -8.0,
                    to_y: 8.0,
                    from_value: -4.0,
                    to_value: 4.0,
                }),
                thresholds: (0..8).map(f64::from).collect(),
                functions,
            }),
            upper_bound: b(Density::Const(8.0)),
            lower_bound: 0,
            cell_height: 8,
        };
        let program = PointProgram::compile(&density);
        let contexts: Vec<_> = (0..17).map(|x| Context::new(x - 8, 0, x)).collect();
        let mut output = vec![0.0; contexts.len()];
        let mut scratch = PointScratch::with_capacity(4);
        program.compute_batch(&contexts, &mut output, &mut scratch);
        for (context, value) in contexts.iter().copied().zip(output) {
            assert_eq!(value.to_bits(), density.compute(context).to_bits());
        }
    }
}
