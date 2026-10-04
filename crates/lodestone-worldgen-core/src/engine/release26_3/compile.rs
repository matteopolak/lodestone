//! Turning an expression tree into a sampler graph.
//!
//! Compilation is two passes, in this order, because the second depends on what
//! the first produced:
//!
//! 1. **Reference/cache pass.** Registry references are inlined. Every `cache`
//!    is replaced by a deduplicated [`Node::Prepared`] leaf: two caches whose raw
//!    (not yet inlined) inputs are structurally equal share one cell. Each cache's
//!    own input is fully optimised and compiled when the cache is first met.
//! 2. **Slice pass.** A function that does not vary along an axis its parent
//!    does is wrapped in a slice on that axis, so it is sampled on a lattice that
//!    is one point wide there.
//!
//! Compile-time shortcuts (constant folding, `min`/`max` that cannot overlap)
//! read ranges of the *rewritten* nodes.

use std::collections::HashMap;

use super::interval::Interval;
use super::noise::{blended_fbm_set, NoiseStack, Octave, Simplex};
use super::sampler::{GradientMode, Program, SplineS, UnaryOp, S, SId};
use super::tree::{
    Axes, Axis, Binary, Node, NodeId, PreparedInfo, ShiftKind, Spline, Tree, Unary, ALL_AXES,
};
use crate::rng::{Algorithm, AnyPositionalFactory, LegacyRandomSource, PositionalRandomFactory, RandomSource};

/// How noise instances are seeded.
#[derive(Clone, Copy, Debug)]
pub struct RandomConfig {
    pub seed: i64,
    pub legacy: bool,
}

#[derive(Clone, Copy)]
enum Rule {
    Reference,
    Slice(Axes),
}

pub struct Compiler {
    pub tree: Tree,
    pub program: Program,
    config: RandomConfig,
    factory: AnyPositionalFactory,
    noise_index: HashMap<u32, usize>,
    /// Raw cache input -> its `Prepared` leaf.
    prepared_by_input: HashMap<NodeId, NodeId>,
    reference_memo: HashMap<NodeId, NodeId>,
    slice_memo: HashMap<(Axes, NodeId), NodeId>,
    compiled: HashMap<NodeId, SId>,
    roots: HashMap<NodeId, SId>,
    simplex: Option<usize>,
}

impl Compiler {
    pub fn new(tree: Tree, config: RandomConfig) -> Self {
        let algorithm = Algorithm::from_legacy_flag(config.legacy);
        Self {
            tree,
            program: Program::default(),
            config,
            factory: algorithm.root_positional(config.seed),
            noise_index: HashMap::new(),
            prepared_by_input: HashMap::new(),
            reference_memo: HashMap::new(),
            slice_memo: HashMap::new(),
            compiled: HashMap::new(),
            roots: HashMap::new(),
            simplex: None,
        }
    }

    pub fn finish(mut self) -> Program {
        self.program.cache_count = self.tree.prepared.len();
        self.program
    }

    /// A positional factory forked from the world seed, by resource name.
    pub fn factory(&self) -> AnyPositionalFactory {
        self.factory
    }

    /// The compiled sampler for a root function (compiling it on first use).
    pub fn sampler(&mut self, function: NodeId) -> SId {
        if let Some(&s) = self.roots.get(&function) {
            return s;
        }
        let optimised = self.optimise(function);
        let s = self.compile(optimised);
        self.roots.insert(function, s);
        s
    }

    fn optimise(&mut self, function: NodeId) -> NodeId {
        let first = self.rewrite(Rule::Reference, function);
        self.rewrite(Rule::Slice(ALL_AXES), first)
    }

    // ------------------------------------------------------------ rewriting

    fn rewrite(&mut self, rule: Rule, node: NodeId) -> NodeId {
        match rule {
            Rule::Reference => self.rewrite_reference(node),
            Rule::Slice(parent) => self.rewrite_slice(parent, node),
        }
    }

    fn rewrite_reference(&mut self, original: NodeId) -> NodeId {
        if let Some(&r) = self.reference_memo.get(&original) {
            return r;
        }
        let mut node = original;
        while let Node::Ref(name) = self.tree.node(node) {
            node = self.tree.registry_value(*name);
        }
        let out = if matches!(self.tree.node(node), Node::Cache(_)) {
            self.reuse_or_prepare(node)
        } else {
            self.map_children(Rule::Reference, node)
        };
        self.reference_memo.insert(original, out);
        out
    }

    fn reuse_or_prepare(&mut self, cache: NodeId) -> NodeId {
        let Node::Cache(raw_input) = self.tree.node(cache).clone() else { unreachable!() };
        if let Some(&p) = self.prepared_by_input.get(&raw_input) {
            return p;
        }
        let id = self.tree.prepared.len() as u32;
        self.tree.prepared.push(PreparedInfo { range: Interval::NAI, axes: 0, sampler: 0 });
        let input = self.optimise(raw_input);
        let range = self.tree.range(input);
        let axes = self.tree.axes(input);
        let inner = self.compile(input);
        let cached = self.push(S::Cached { id: id as usize, input: inner });
        self.tree.prepared[id as usize] = PreparedInfo { range, axes, sampler: cached };
        let prepared = self.tree.intern(Node::Prepared(id));
        self.prepared_by_input.insert(raw_input, prepared);
        prepared
    }

    fn rewrite_slice(&mut self, parent: Axes, node: NodeId) -> NodeId {
        if let Some(&r) = self.slice_memo.get(&(parent, node)) {
            return r;
        }
        let out = if matches!(self.tree.node(node), Node::Const(_) | Node::Gradient { .. }) {
            node
        } else {
            let axes = self.tree.axes(node);
            if parent == axes {
                self.map_children(Rule::Slice(parent), node)
            } else {
                let rewritten = self.map_children(Rule::Slice(axes), node);
                self.remove_axes(rewritten, parent & !axes)
            }
        };
        self.slice_memo.insert((parent, node), out);
        out
    }

    fn remove_axes(&mut self, mut node: NodeId, axes: Axes) -> NodeId {
        let mut existing: Axes = 0;
        let mut probe = node;
        while let Node::Slice { axis, input, .. } = self.tree.node(probe) {
            existing |= axis.mask();
            probe = *input;
        }
        let filtered = axes & !existing;
        if filtered & 1 != 0 {
            node = self.tree.intern(Node::Slice { axis: Axis::X, coordinate: 0, input: node });
        }
        if filtered & 4 != 0 {
            node = self.tree.intern(Node::Slice { axis: Axis::Z, coordinate: 0, input: node });
        }
        if filtered & 2 != 0 {
            node = self.tree.intern(Node::Slice { axis: Axis::Y, coordinate: 0, input: node });
        }
        node
    }

    /// Rebuilds a node with every child passed through the rule, in the order the
    /// reference visits them.
    fn map_children(&mut self, rule: Rule, id: NodeId) -> NodeId {
        let node = self.tree.node(id).clone();
        let r = |c: &mut Self, child: NodeId| c.rewrite(rule, child);
        let rebuilt = match node {
            Node::Noise { noise, xz_scale, y_scale, shift } => {
                let s = [r(self, shift[0]), r(self, shift[1]), r(self, shift[2])];
                Node::Noise { noise, xz_scale, y_scale, shift: s }
            }
            Node::Unary(k, i) => Node::Unary(k, r(self, i)),
            Node::Cache(i) => Node::Cache(r(self, i)),
            Node::BlendDensity(i) => Node::BlendDensity(r(self, i)),
            Node::Interpolated { input, cell_xz, cell_y } => Node::Interpolated { input: r(self, input), cell_xz, cell_y },
            Node::Slice { axis, coordinate, input } => Node::Slice { axis, coordinate, input: r(self, input) },
            Node::Clamp { input, min, max } => Node::Clamp { input: r(self, input), min, max },
            Node::Round { kind, input, multiple } => {
                let i = r(self, input);
                Node::Round { kind, input: i, multiple: r(self, multiple) }
            }
            Node::Binary(k, l, rt) => {
                let a = r(self, l);
                Node::Binary(k, a, r(self, rt))
            }
            Node::Pow(b, e) => {
                let a = r(self, b);
                Node::Pow(a, r(self, e))
            }
            Node::Lerp(a, f, s) => {
                let a = r(self, a);
                let f = r(self, f);
                Node::Lerp(a, f, r(self, s))
            }
            Node::RangeChoice { input, min_inclusive, max_exclusive, in_range, out_of_range } => {
                let i = r(self, input);
                let a = r(self, in_range);
                Node::RangeChoice { input: i, min_inclusive, max_exclusive, in_range: a, out_of_range: r(self, out_of_range) }
            }
            Node::IntervalSelect { input, thresholds, functions } => {
                let i = r(self, input);
                let f = functions.iter().map(|&f| r(self, f)).collect();
                Node::IntervalSelect { input: i, thresholds, functions: f }
            }
            Node::FindTopSurface { density, upper_bound, lower_bound, cell_height } => {
                let d = r(self, density);
                Node::FindTopSurface { density: d, upper_bound: r(self, upper_bound), lower_bound, cell_height }
            }
            Node::Spline(spline) => {
                let mut map = |c: NodeId| r(self, c);
                Node::Spline(spline.map_coordinates(&mut map))
            }
            other => other,
        };
        self.tree.intern(rebuilt)
    }

    // ------------------------------------------------------------ compiling

    fn push(&mut self, s: S) -> SId {
        self.program.samplers.push(s);
        (self.program.samplers.len() - 1) as SId
    }

    fn constant_of(&self, id: NodeId) -> Option<f32> {
        match self.tree.node(id) {
            Node::Const(b) => Some(f32::from_bits(*b)),
            _ => None,
        }
    }

    /// The program noise index of a named noise loaded into the tree with
    /// [`Tree::load_noise`], created like any density-function noise.
    pub fn named_noise(&mut self, name: u32) -> usize {
        self.noise_stack(name)
    }

    fn noise_stack(&mut self, name: u32) -> usize {
        if let Some(&i) = self.noise_index.get(&name) {
            return i;
        }
        let key = self.tree.name(name).to_owned();
        let def = self.tree.noise_def(name).clone();
        let stack = if key == "minecraft:nether/temperature" {
            def.create_for_legacy_nether_biome(&mut LegacyRandomSource::new(self.config.seed))
        } else if key == "minecraft:nether/vegetation" {
            def.create_for_legacy_nether_biome(&mut LegacyRandomSource::new(self.config.seed.wrapping_add(1)))
        } else {
            def.create(&mut self.factory.from_hash_of(&key))
        };
        let index = self.add_noise(stack);
        self.noise_index.insert(name, index);
        index
    }

    fn add_noise(&mut self, stack: NoiseStack) -> usize {
        self.program.noises.push(stack);
        self.program.noises.len() - 1
    }

    fn simplex_index(&mut self) -> usize {
        if let Some(i) = self.simplex {
            return i;
        }
        let mut random = LegacyRandomSource::new(self.config.seed);
        random.consume_count(17292);
        self.program.simplexes.push(Simplex::new_discarding_offset(&mut random));
        let i = self.program.simplexes.len() - 1;
        self.simplex = Some(i);
        i
    }

    pub fn compile(&mut self, node: NodeId) -> SId {
        if let Some(&s) = self.compiled.get(&node) {
            return s;
        }
        let s = self.compile_uncached(node);
        self.compiled.insert(node, s);
        s
    }

    fn compile_uncached(&mut self, id: NodeId) -> SId {
        let node = self.tree.node(id).clone();
        match node {
            Node::Const(b) => self.push(S::Const(f32::from_bits(b))),
            Node::Ref(_) | Node::Cache(_) => panic!("unresolved reference or cache reached the compiler"),
            Node::Prepared(p) => self.tree.prepared[p as usize].sampler,
            Node::Context(k) => self.push(S::Context(k)),
            Node::Noise { noise, xz_scale, y_scale, shift } => {
                let stack = self.noise_stack(noise);
                let (xz, y) = (f64::from_bits(xz_scale), f64::from_bits(y_scale));
                let zero = self.tree.constant(0.0);
                if shift.iter().all(|&s| s == zero) {
                    return self.push(S::Noise { noise: stack, xz, y });
                }
                let shift_x = self.compile(shift[0]);
                let shift_z = self.compile(shift[2]);
                if shift[1] == zero {
                    return self.push(S::ShiftedXz { shift_x, shift_z, noise: stack, xz, y });
                }
                let shift_y = self.compile(shift[1]);
                self.push(S::ShiftedXyz { shift_x, shift_y, shift_z, noise: stack, xz, y })
            }
            Node::Shift { kind, noise } => {
                let stack = self.noise_stack(noise);
                match kind {
                    ShiftKind::Shift => {
                        let n = self.push(S::Noise { noise: stack, xz: 0.25, y: 0.25 });
                        self.push(S::ConstMul(n, 4.0))
                    }
                    ShiftKind::A => {
                        let n = self.push(S::Noise { noise: stack, xz: 0.25, y: 0.0 });
                        self.push(S::ConstMul(n, 4.0))
                    }
                    ShiftKind::B => self.push(S::ShiftB { noise: stack }),
                }
            }
            Node::EndIslands => {
                let simplex = self.simplex_index();
                self.push(S::EndIslands { simplex })
            }
            Node::DistanceToPoint { point, metric } => self.push(S::Distance { point, metric }),
            Node::Gradient { axis, tiling, from, to, from_value, to_value } => {
                let (fv, tv) = (f32::from_bits(from_value), f32::from_bits(to_value));
                let range = to.wrapping_sub(from);
                let factor = (tv - fv) / range as f32;
                let mode: GradientMode = tiling.mode(from, to);
                self.push(S::Gradient { axis, from, from_value: fv, factor, mode })
            }
            Node::Unary(kind, input) => {
                let i = self.compile(input);
                match kind {
                    Unary::Abs => self.push(S::Unary(UnaryOp::Abs, i)),
                    Unary::Square => self.push(S::Unary(UnaryOp::Square, i)),
                    Unary::Cube => self.push(S::Unary(UnaryOp::Cube, i)),
                    Unary::Sqrt => self.push(S::Unary(UnaryOp::Sqrt, i)),
                    Unary::HalfNegative => self.push(S::Leaky(i, 0.5)),
                    Unary::QuarterNegative => self.push(S::Leaky(i, 0.25)),
                    Unary::Reciprocal => self.push(S::Unary(UnaryOp::Reciprocal, i)),
                    Unary::Negate => self.push(S::Unary(UnaryOp::Negate, i)),
                    Unary::Squeeze => self.push(S::Unary(UnaryOp::Squeeze, i)),
                    Unary::Log => self.push(S::Unary(UnaryOp::Log, i)),
                    Unary::Sign => self.push(S::Unary(UnaryOp::Sign, i)),
                }
            }
            Node::Round { kind, input, multiple } => {
                let i = self.compile(input);
                if self.constant_of(multiple) == Some(1.0) {
                    return self.push(S::Round { kind, input: i, multiple: None });
                }
                let m = self.compile(multiple);
                self.push(S::Round { kind, input: i, multiple: Some(m) })
            }
            Node::Binary(kind, l, r) => self.compile_binary(kind, l, r),
            Node::Pow(b, e) => {
                let base = self.compile(b);
                let exponent = self.compile(e);
                if let Some(bv) = self.constant_of(b) {
                    return self.push(S::PowConstBase(f64::from(bv), exponent));
                }
                if let Some(ev) = self.constant_of(e) {
                    return self.compile_const_exponent(base, ev);
                }
                self.push(S::Pow(base, exponent))
            }
            Node::Spline(spline) => {
                let mut coordinates: Vec<NodeId> = Vec::new();
                let built = self.build_spline(&spline, &mut coordinates);
                let compiled = coordinates.iter().map(|&c| self.compile(c)).collect();
                self.push(S::Spline { spline: built, coordinates: compiled })
            }
            Node::Lerp(a, f, s) => {
                let alpha = self.compile(a);
                let first = self.compile(f);
                let second = self.compile(s);
                if let Some(fv) = self.constant_of(f) {
                    return self.push(S::LerpConstFirst { alpha, first: fv, second });
                }
                if let Some(sv) = self.constant_of(s) {
                    return self.push(S::LerpConstSecond { alpha, first, second: sv });
                }
                self.push(S::Lerp { alpha, first, second })
            }
            Node::Clamp { input, min, max } => {
                let i = self.compile(input);
                self.push(S::Clamp { input: i, min: f32::from_bits(min), max: f32::from_bits(max) })
            }
            Node::RangeChoice { input, min_inclusive, max_exclusive, in_range, out_of_range } => {
                let i = self.compile(input);
                let (min, max) = (f32::from_bits(min_inclusive), f32::from_bits(max_exclusive));
                if let (Some(a), Some(b)) = (self.constant_of(in_range), self.constant_of(out_of_range)) {
                    return self.push(S::RangeChoiceConst { input: i, min, max, in_range: a, out_of_range: b });
                }
                let a = self.compile(in_range);
                let b = self.compile(out_of_range);
                self.push(S::RangeChoice { input: i, min, max, in_range: a, out_of_range: b })
            }
            Node::IntervalSelect { input, thresholds, functions } => {
                let i = self.compile(input);
                let thresholds: Vec<f32> = thresholds.iter().map(|&b| f32::from_bits(b)).collect();
                if thresholds.len() == 1 {
                    let below = self.compile(functions[0]);
                    let above = self.compile(functions[functions.len() - 1]);
                    return self.push(S::SelectSingle { input: i, threshold: thresholds[0], below, above });
                }
                let samplers = functions.iter().map(|&f| self.compile(f)).collect();
                self.push(S::SelectMulti { input: i, thresholds, samplers })
            }
            Node::BlendDensity(input) => {
                let i = self.compile(input);
                self.push(S::BlendDensity(i))
            }
            Node::Interpolated { input, cell_xz, cell_y } => {
                let i = self.compile(input);
                self.push(S::Interpolated {
                    input: i,
                    cell_xz,
                    cell_y,
                    inv_xz: 1.0 / cell_xz as f32,
                    inv_y: 1.0 / cell_y as f32,
                })
            }
            Node::Slice { axis, coordinate, input } => {
                if let Node::Slice { axis: inner_axis, coordinate: inner_coordinate, input: inner_input } = *self.tree.node(input) {
                    let pair = matches!((axis, inner_axis), (Axis::X, Axis::Z) | (Axis::Z, Axis::X));
                    if pair {
                        let (x, z) = if axis == Axis::X { (coordinate, inner_coordinate) } else { (inner_coordinate, coordinate) };
                        let i = self.compile(inner_input);
                        return self.push(S::SliceXz { x, z, input: i });
                    }
                }
                let i = self.compile(input);
                self.push(S::Slice { axis, coordinate, input: i })
            }
            Node::FindTopSurface { density, upper_bound, lower_bound, cell_height } => {
                let d = self.compile(density);
                let u = self.compile(upper_bound);
                let inner = self.push(S::FindTopSurface { density: d, upper_bound: u, lower_bound, cell_height });
                self.push(S::Slice { axis: Axis::Y, coordinate: 0, input: inner })
            }
            Node::OldBlendedNoise { xz_scale, y_scale, xz_factor, y_factor, smear } => {
                let (xz_scale, y_scale) = (f64::from_bits(xz_scale), f64::from_bits(y_scale));
                let (xz_factor, y_factor, smear) = (f64::from_bits(xz_factor), f64::from_bits(y_factor), f64::from_bits(smear));
                let stacks = if self.config.legacy {
                    blended_fbm_set(&mut LegacyRandomSource::new(self.config.seed), y_scale, smear, y_factor)
                } else {
                    blended_fbm_set(&mut self.factory.from_hash_of("minecraft:terrain"), y_scale, smear, y_factor)
                };
                let xz_mult = 684.412 * xz_scale;
                let y_mult = 684.412 * y_scale;
                let min = self.add_noise(stacks.min_limit);
                let max = self.add_noise(stacks.max_limit);
                let main = self.add_noise(stacks.main);
                let min_s = self.push(S::Noise { noise: min, xz: xz_mult, y: y_mult });
                let max_s = self.push(S::Noise { noise: max, xz: xz_mult, y: y_mult });
                let main_s = self.push(S::Noise { noise: main, xz: xz_mult / xz_factor, y: y_mult / y_factor });
                let shifted = self.push(S::ConstAdd(main_s, 0.5));
                let choice = self.push(S::Clamp { input: shifted, min: 0.0, max: 1.0 });
                self.push(S::Lerp { alpha: choice, first: min_s, second: max_s })
            }
        }
    }

    fn compile_const_exponent(&mut self, base: SId, exponent: f32) -> SId {
        let abs = exponent.abs();
        let special = if abs == 0.5 {
            self.push(S::Unary(UnaryOp::Sqrt, base))
        } else if abs == 1.0 {
            base
        } else if abs == 2.0 {
            self.push(S::Unary(UnaryOp::Square, base))
        } else if abs == 3.0 {
            self.push(S::Unary(UnaryOp::Cube, base))
        } else {
            return self.push(S::PowConstExponent(base, f64::from(exponent)));
        };
        if exponent >= 0.0 { special } else { self.push(S::Unary(UnaryOp::Reciprocal, special)) }
    }

    fn compile_binary(&mut self, kind: Binary, l: NodeId, r: NodeId) -> SId {
        let left = self.compile(l);
        let right = self.compile(r);
        let (lc, rc) = (self.constant_of(l), self.constant_of(r));
        match kind {
            Binary::Add => {
                if let Some(v) = lc {
                    self.push(S::ConstAdd(right, v))
                } else if let Some(v) = rc {
                    self.push(S::ConstAdd(left, v))
                } else {
                    self.push(S::Add(left, right))
                }
            }
            Binary::Sub => {
                if let Some(v) = lc {
                    self.push(S::ConstSub(v, right))
                } else if let Some(v) = rc {
                    self.push(S::ConstAdd(left, -v))
                } else {
                    self.push(S::Sub(left, right))
                }
            }
            Binary::Mul => {
                if let Some(v) = lc {
                    self.push(S::ConstMul(right, v))
                } else if let Some(v) = rc {
                    self.push(S::ConstMul(left, v))
                } else {
                    self.push(S::Mul(left, right))
                }
            }
            Binary::Div => {
                if let Some(v) = lc {
                    self.push(S::ConstDiv(v, right))
                } else if let Some(v) = rc {
                    self.push(S::ConstMul(left, 1.0 / v))
                } else {
                    self.push(S::Div(left, right))
                }
            }
            Binary::Min => {
                let (lr, rr) = (self.tree.range(l), self.tree.range(r));
                if lr.max() < rr.min() {
                    left
                } else if rr.max() < lr.min() {
                    right
                } else if let Some(v) = lc {
                    self.push(S::ConstMin(right, v))
                } else if let Some(v) = rc {
                    self.push(S::ConstMin(left, v))
                } else {
                    self.push(S::Min { left, right, right_min: rr.min() })
                }
            }
            Binary::Max => {
                let (lr, rr) = (self.tree.range(l), self.tree.range(r));
                if lr.min() > rr.max() {
                    left
                } else if rr.min() > lr.max() {
                    right
                } else if let Some(v) = lc {
                    self.push(S::ConstMax(right, v))
                } else if let Some(v) = rc {
                    self.push(S::ConstMax(left, v))
                } else {
                    self.push(S::Max { left, right, right_max: rr.max() })
                }
            }
        }
    }

    /// Maps a spline to its sampler form, collecting distinct coordinate functions
    /// in first-visit order.
    fn build_spline(&mut self, spline: &Spline, coordinates: &mut Vec<NodeId>) -> SplineS {
        match spline {
            Spline::Const(b) => SplineS::Const(f32::from_bits(*b)),
            Spline::Multi { coordinate, locations, values, derivatives } => {
                let index = match coordinates.iter().position(|c| c == coordinate) {
                    Some(i) => i,
                    None => {
                        coordinates.push(*coordinate);
                        coordinates.len() - 1
                    }
                };
                let values = values.iter().map(|v| self.build_spline(v, coordinates)).collect();
                SplineS::Multi {
                    coordinate: index,
                    locations: locations.iter().map(|b| f32::from_bits(*b)).collect(),
                    values,
                    derivatives: derivatives.iter().map(|b| f32::from_bits(*b)).collect(),
                }
            }
        }
    }
}

#[allow(dead_code)]
fn _octave_marker(_: Octave) {}

impl std::fmt::Debug for Compiler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compiler").finish_non_exhaustive()
    }
}
