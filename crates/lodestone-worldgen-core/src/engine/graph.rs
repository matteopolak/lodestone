//! The flattened, index-addressed density graph.
//!
//! [`Graph`] is a compiled form of a [`Density`] tree: every node is a
//! fixed-size [`Op`] in one `Vec`, and every child reference is a `u32` index
//! into that same `Vec` rather than a `Box`. Variable-width payloads (floats,
//! child lists, instantiated noises, opaque point-evaluated leaves) live in
//! side tables, also indexed by `u32`.
//!
//! # Why
//!
//! `Density` is a *wide* enum: its largest variant inlines a `BlendedNoise`
//! (three `PerlinNoise` stacks, each two `Vec`s plus two `f64`s), so **every**
//! node — including a bare `Const(f64)` — occupies that width, and every child
//! is a separate heap allocation of it. [`tests::density_node_is_much_wider_than_an_op`]
//! pins the measured ratio. Flattening buys three things:
//!
//! 1. **Locality.** The whole graph is one contiguous `Vec<Op>` walked by index.
//! 2. **Cheap sharing.** A [`Program`] is `Arc<Graph>` + a root index, so
//!    handing the same graph to another chunk or thread is a refcount bump
//!    instead of a recursive deep copy. That is diagnostic D3's eight
//!    per-chunk `Density` clones, deleted.
//! 3. **Cache state moves out of the graph.** All mutable memoisation lives in
//!    [`super::Scratch`], so the graph itself is immutable and `Sync` with no
//!    interior mutability at all.
//!
//! # What it deliberately does *not* flatten
//!
//! `spline`, `old_blended_noise` and `find_top_surface` are **leaves** to the
//! block-field evaluator: it does not recurse into them, it calls the *point*
//! interpreter ([`Density::compute`]). Everything beneath one of those is
//! therefore evaluated with point semantics — no quart snapping, no
//! interpolation. That is a real semantic, not an optimisation
//! (`docs/worldgen-density-engine.md`), so those three kinds compile to an
//! index into [`Graph::leaves`], which holds the original `Density` subtree
//! untouched. Flattening beneath them would silently change their semantics.
//!
//! # Node kind fidelity
//!
//! [`OpKind`]'s discriminants **are** [`Density::kind_index`]'s values, so the
//! `density_evals` per-kind counter reads the same bucket before and after the
//! flattening and the D1 diagnostic stays comparable across the cutover.
//! [`tests::op_kind_discriminants_match_density_kind_index`] is that gate — and
//! it is the gate that would catch a flattening pass mislabelling a node,
//! which is otherwise invisible (a mislabelled node still *evaluates*, it just
//! evaluates as the wrong operator).

use std::sync::Arc;

use crate::density::{Density, Spline};
use crate::noise::NormalNoise;

/// An index into [`Graph::ops`].
pub type NodeId = u32;

/// The operator of one flattened node.
///
/// Discriminants are deliberately equal to [`Density::kind_index`] — see the
/// module doc's *Node kind fidelity*. Do not renumber; the per-kind counter
/// tables in [`crate::counters`] are indexed by these values and a recorded
/// table from an earlier run is indexed by the same numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum OpKind {
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
}

/// One flattened node: an operator plus up to three `u32` payload slots.
///
/// The meaning of `a`/`b`/`c` is per-[`OpKind`] and documented on
/// [`Graph::compile_node`], which is the only writer. Keeping the node
/// fixed-width and small (16 bytes) is the point of the exercise, so
/// wide payloads are always an index into a side table rather than inline.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Op {
    pub(crate) kind: OpKind,
    pub(crate) a: u32,
    pub(crate) b: u32,
    pub(crate) c: u32,
}

/// A compiled density graph: immutable, `Sync`, and shared by `Arc`.
///
/// Holds no mutable state whatsoever — every cache lives in
/// [`super::Scratch`]. That is what makes [`Program`]'s clone a refcount bump
/// and what lets one graph back concurrent chunk generation on many threads
/// without a lock.
#[allow(missing_debug_implementations)]
pub struct Graph {
    ops: Vec<Op>,
    /// Inline `f64` payloads (constants, scales, bounds, thresholds), addressed
    /// by offset+known-arity from an [`Op`].
    params: Vec<f64>,
    /// Child-id runs for the operators with more than two children
    /// (`shifted_noise`, `range_choice`, `interval_select`).
    children: Vec<NodeId>,
    /// Instantiated noises, kept out of [`Op`] because a `NormalNoise` is two
    /// `PerlinNoise` stacks — the very payload whose inlining makes `Density`
    /// wide.
    noises: Vec<NormalNoise>,
    /// `spline` / `old_blended_noise` / `find_top_surface` subtrees, held as
    /// the original `Density` because the block-field evaluator treats them as
    /// opaque point-evaluated leaves (module doc).
    leaves: Vec<Density>,
}

/// A root into a shared [`Graph`] — the unit callers hold and clone.
///
/// Cloning is an `Arc` bump plus a `u32` copy. This is the type that replaces
/// a per-chunk `Density` deep clone.
#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub struct Program {
    graph: Arc<Graph>,
    root: NodeId,
}

impl Program {
    /// Compiles `root` into a fresh single-root graph.
    ///
    /// Deliberately does **not** take a slot count. The number of cache slots is
    /// a property of the *scratch* a sampler evaluates against, not of the graph,
    /// and conflating them creates a construction-order trap: `Builder`'s
    /// `slot_count` is an over-approximation shared across every tree it built
    /// and is only final after the **last** `build` call, so a `compile` that
    /// demanded it could not be called at the point where the trees are
    /// assembled. It is supplied to
    /// [`NoiseChunkSampler::from_program`](crate::density::NoiseChunkSampler::from_program)
    /// instead.
    #[must_use]
    pub fn compile(root: &Density) -> Self {
        let mut g = Graph {
            ops: Vec::new(),
            params: Vec::new(),
            children: Vec::new(),
            noises: Vec::new(),
            leaves: Vec::new(),
        };
        let id = g.compile_node(root);
        Self {
            graph: Arc::new(g),
            root: id,
        }
    }

    /// The shared graph.
    #[must_use]
    pub(crate) fn graph(&self) -> &Graph {
        &self.graph
    }

    /// This program's root node.
    #[must_use]
    pub(crate) fn root(&self) -> NodeId {
        self.root
    }

    /// Number of flattened nodes in the shared graph — an implementation
    /// detail exposed only so gates can assert the flattening actually
    /// happened (a graph of one node would pass every value assertion while
    /// having flattened nothing).
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.graph.ops.len()
    }

    /// Number of opaque point-evaluated leaves (`spline`, `old_blended_noise`,
    /// `find_top_surface`). Exposed for the same reason as
    /// [`node_count`](Self::node_count): a gate needs to be able to see that
    /// the leaf boundary exists where the semantics say it does.
    #[must_use]
    pub fn leaf_count(&self) -> usize {
        self.graph.leaves.len()
    }

    /// Counts nodes of one JSON type name (as spelled in
    /// [`Density::KIND_NAMES`]) reachable in the flattened region of this
    /// graph. Used by gates that need to prove a semantic applies to real
    /// router data rather than only to a hand-built fixture — e.g. that the
    /// compiled `final_density` really does contain an `interpolated` node,
    /// without which every interpolation assertion is vacuous.
    ///
    /// Nodes *inside* a point-evaluated leaf are deliberately not counted:
    /// they are not part of the flattened graph.
    #[must_use]
    pub fn count_kind(&self, kind_name: &str) -> usize {
        self.graph
            .ops
            .iter()
            .filter(|op| Density::KIND_NAMES[op.kind as usize] == kind_name)
            .count()
    }

    /// The cache slots of every `interpolated` node reachable from the root
    /// **with `interpolate == true`** — i.e. the ones that actually perform
    /// corner lookups.
    ///
    /// Not the same as [`count_kind`](Self::count_kind)`("interpolated")`, and
    /// the difference is load-bearing: the real overworld `final_density`
    /// contains five `interpolated` nodes but only some are reached in an
    /// interpolating context. A nested one is **transparent** (its enclosing
    /// `interpolated` or `flat_cache` evaluates its inner with
    /// `interpolate = false`), so it fetches no corners and fills no cells, and
    /// a corner-lookup prediction built by counting `interpolated` nodes in the
    /// data would be wrong by whatever that ratio happens to be. This walk
    /// applies the same transparency rule the evaluator does.
    ///
    /// It is a **structural upper bound on participation**, not an exact
    /// per-query set: `Mul` may skip its second operand and
    /// `range_choice`/`interval_select` take one branch per position, so a slot
    /// reachable here can still fill fewer than every cell. Slots are returned
    /// sorted and deduplicated.
    #[must_use]
    pub fn interpolating_slots(&self) -> Vec<u32> {
        let mut out = Vec::new();
        self.graph.walk_interpolating(self.root, true, &mut out);
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Counts `cache_2d` nodes nested inside this graph's point-evaluated
    /// leaves.
    ///
    /// Load-bearing for the sharing decision, not a curiosity: a
    /// [`Density::Cache2D`] carries a `Mutex`-backed last-value slot, so if any
    /// existed under a leaf, `Arc`-sharing one graph across threads would turn
    /// a per-chunk cold cache into a contended shared one. Value-invariant
    /// either way (the memo is keyed on an exact `(x, z)` and the function is
    /// pure), but a lock is not free, so the count is asserted rather than
    /// assumed — see `docs/worldgen-density-engine.md`.
    #[must_use]
    pub fn cache_2d_under_leaves(&self) -> usize {
        self.graph
            .leaves
            .iter()
            .map(|d| count_cache_2d(d))
            .sum()
    }
}

fn count_cache_2d(d: &Density) -> usize {
    let here = usize::from(matches!(d, Density::Cache2D { .. }));
    here + child_densities(d).into_iter().map(count_cache_2d).sum::<usize>()
}

/// Every direct `Density` child of a node, for the structural walks above.
/// Deliberately a `Vec` rather than an iterator: this runs once per gate, not
/// per block.
fn child_densities(d: &Density) -> Vec<&Density> {
    match d {
        Density::Const(_)
        | Density::BlendAlpha
        | Density::BlendOffset
        | Density::Beardifier
        | Density::YClampedGradient { .. }
        | Density::Noise { .. }
        | Density::ShiftA(_)
        | Density::ShiftB(_)
        | Density::Shift(_)
        | Density::Blended(_) => Vec::new(),
        Density::Add(a, b) | Density::Mul(a, b) | Density::Min(a, b) | Density::Max(a, b) => {
            vec![a, b]
        }
        Density::Abs(a)
        | Density::Square(a)
        | Density::Cube(a)
        | Density::HalfNegative(a)
        | Density::QuarterNegative(a)
        | Density::Squeeze(a)
        | Density::Invert(a)
        | Density::Marker(a) => vec![a],
        Density::Clamp { input, .. } => vec![input],
        Density::Interpolated { inner, .. }
        | Density::FlatCache { inner, .. }
        | Density::Cache2D { inner, .. } => vec![inner],
        Density::ShiftedNoise {
            shift_x,
            shift_y,
            shift_z,
            ..
        } => vec![shift_x, shift_y, shift_z],
        Density::RangeChoice {
            input,
            when_in_range,
            when_out_of_range,
            ..
        } => vec![input, when_in_range, when_out_of_range],
        Density::IntervalSelect {
            input, functions, ..
        } => {
            let mut v = vec![&**input];
            v.extend(functions.iter());
            v
        }
        Density::Spline(s) => spline_children(s),
        Density::FindTopSurface {
            density,
            upper_bound,
            ..
        } => vec![density, upper_bound],
    }
}

fn spline_children(s: &Spline) -> Vec<&Density> {
    match s {
        Spline::Constant(_) => Vec::new(),
        Spline::Multipoint { coordinate, points } => {
            let mut v = vec![&**coordinate];
            for p in points {
                v.extend(spline_children(&p.value));
            }
            v
        }
    }
}

impl Graph {
    pub(crate) fn op(&self, id: NodeId) -> Op {
        self.ops[id as usize]
    }

    pub(crate) fn param(&self, at: u32) -> f64 {
        self.params[at as usize]
    }

    pub(crate) fn child(&self, at: u32) -> NodeId {
        self.children[at as usize]
    }

    pub(crate) fn noise(&self, at: u32) -> &NormalNoise {
        &self.noises[at as usize]
    }

    pub(crate) fn leaf(&self, at: u32) -> &Density {
        &self.leaves[at as usize]
    }

    fn push(&mut self, kind: OpKind, a: u32, b: u32, c: u32) -> NodeId {
        let id = self.ops.len() as NodeId;
        self.ops.push(Op { kind, a, b, c });
        id
    }

    fn push_params(&mut self, vals: &[f64]) -> u32 {
        let at = self.params.len() as u32;
        self.params.extend_from_slice(vals);
        at
    }

    fn push_children(&mut self, ids: &[NodeId]) -> u32 {
        let at = self.children.len() as u32;
        self.children.extend_from_slice(ids);
        at
    }

    fn push_noise(&mut self, n: &NormalNoise) -> u32 {
        let at = self.noises.len() as u32;
        self.noises.push(n.clone());
        at
    }

    fn push_leaf(&mut self, d: &Density) -> u32 {
        let at = self.leaves.len() as u32;
        self.leaves.push(d.clone());
        at
    }

    /// Compiles one `Density` node and its children, post-order, returning the
    /// new node's id. Children therefore always have *lower* ids than their
    /// parent — a property the evaluator does not rely on (it is a recursive
    /// descent, because `Mul`'s short-circuit forbids a bottom-up sweep) but
    /// which makes a compiled graph readable in a dump.
    ///
    /// Payload conventions, by kind:
    ///
    /// | kind | `a` | `b` | `c` |
    /// |---|---|---|---|
    /// | `Const` | params\[1\] | — | — |
    /// | `BlendAlpha`/`BlendOffset`/`Beardifier` | — | — | — |
    /// | `YClampedGradient` | params\[4\] | — | — |
    /// | `Add`/`Mul`/`Min`/`Max` | lhs id | rhs id | — |
    /// | unary arithmetic, `Marker`, `Cache2D` | child id | — | — |
    /// | `Clamp` | child id | params\[2\] | — |
    /// | `Interpolated`/`FlatCache` | child id | slot | — |
    /// | `Noise` | noise idx | params\[2\] | — |
    /// | `ShiftedNoise` | children\[3\] | noise idx | params\[2\] |
    /// | `ShiftA`/`ShiftB`/`Shift` | noise idx | — | — |
    /// | `RangeChoice` | children\[3\] | params\[2\] | — |
    /// | `IntervalSelect` | children\[n\] | n | params\[n-1\] |
    /// | `Spline`/`Blended`/`FindTopSurface` | leaf idx | — | — |
    fn compile_node(&mut self, d: &Density) -> NodeId {
        match d {
            Density::Const(v) => {
                let p = self.push_params(&[*v]);
                self.push(OpKind::Const, p, 0, 0)
            }
            Density::BlendAlpha => self.push(OpKind::BlendAlpha, 0, 0, 0),
            Density::BlendOffset => self.push(OpKind::BlendOffset, 0, 0, 0),
            Density::Beardifier => self.push(OpKind::Beardifier, 0, 0, 0),
            Density::YClampedGradient {
                from_y,
                to_y,
                from_value,
                to_value,
            } => {
                let p = self.push_params(&[*from_y, *to_y, *from_value, *to_value]);
                self.push(OpKind::YClampedGradient, p, 0, 0)
            }
            Density::Add(a, b) => self.binary(OpKind::Add, a, b),
            Density::Mul(a, b) => self.binary(OpKind::Mul, a, b),
            Density::Min(a, b) => self.binary(OpKind::Min, a, b),
            Density::Max(a, b) => self.binary(OpKind::Max, a, b),
            Density::Abs(a) => self.unary(OpKind::Abs, a),
            Density::Square(a) => self.unary(OpKind::Square, a),
            Density::Cube(a) => self.unary(OpKind::Cube, a),
            Density::HalfNegative(a) => self.unary(OpKind::HalfNegative, a),
            Density::QuarterNegative(a) => self.unary(OpKind::QuarterNegative, a),
            Density::Squeeze(a) => self.unary(OpKind::Squeeze, a),
            Density::Invert(a) => self.unary(OpKind::Invert, a),
            Density::Clamp { input, min, max } => {
                let child = self.compile_node(input);
                let p = self.push_params(&[*min, *max]);
                self.push(OpKind::Clamp, child, p, 0)
            }
            Density::Interpolated { inner, slot } => {
                let child = self.compile_node(inner);
                self.push(OpKind::Interpolated, child, u32::try_from(*slot).unwrap(), 0)
            }
            Density::FlatCache { inner, slot } => {
                let child = self.compile_node(inner);
                self.push(OpKind::FlatCache, child, u32::try_from(*slot).unwrap(), 0)
            }
            // `cache_2d` is transparent in the block field (it is a real memo
            // only in the point interpreter), but the node is still emitted so
            // the `density_evals` per-kind counter keeps reporting it — the
            // flattening is not the place to change what the D1 diagnostic
            // measures.
            Density::Cache2D { inner, .. } => self.unary(OpKind::Cache2D, inner),
            Density::Marker(inner) => self.unary(OpKind::Marker, inner),
            Density::Noise {
                noise,
                xz_scale,
                y_scale,
            } => {
                let n = self.push_noise(noise);
                let p = self.push_params(&[*xz_scale, *y_scale]);
                self.push(OpKind::Noise, n, p, 0)
            }
            Density::ShiftedNoise {
                shift_x,
                shift_y,
                shift_z,
                xz_scale,
                y_scale,
                noise,
            } => {
                let cx = self.compile_node(shift_x);
                let cy = self.compile_node(shift_y);
                let cz = self.compile_node(shift_z);
                let kids = self.push_children(&[cx, cy, cz]);
                let n = self.push_noise(noise);
                let p = self.push_params(&[*xz_scale, *y_scale]);
                self.push(OpKind::ShiftedNoise, kids, n, p)
            }
            Density::ShiftA(n) => {
                let i = self.push_noise(n);
                self.push(OpKind::ShiftA, i, 0, 0)
            }
            Density::ShiftB(n) => {
                let i = self.push_noise(n);
                self.push(OpKind::ShiftB, i, 0, 0)
            }
            Density::Shift(n) => {
                let i = self.push_noise(n);
                self.push(OpKind::Shift, i, 0, 0)
            }
            Density::RangeChoice {
                input,
                min_inclusive,
                max_exclusive,
                when_in_range,
                when_out_of_range,
            } => {
                let ci = self.compile_node(input);
                let cin = self.compile_node(when_in_range);
                let cout = self.compile_node(when_out_of_range);
                let kids = self.push_children(&[ci, cin, cout]);
                let p = self.push_params(&[*min_inclusive, *max_exclusive]);
                self.push(OpKind::RangeChoice, kids, p, 0)
            }
            Density::IntervalSelect {
                input,
                thresholds,
                functions,
            } => {
                // The threshold count is stored explicitly rather than derived
                // as `functions.len() - 1`. The two are equal in well-formed
                // data, but `Builder` tolerates a missing `thresholds` array
                // (`unwrap_or_default`), and the tree walker's loop is over
                // `thresholds`, not over `functions` — so with `k != n - 1` it
                // performs `k` comparisons and falls back to the last function.
                // Deriving `k` here would instead read `n - 1` params, running
                // off the end of this node's params into whatever the next node
                // pushed. Layout: `children[a] = n`, then input, then the n
                // functions; `b` = params offset; `c` = k.
                let ci = self.compile_node(input);
                let n = u32::try_from(functions.len()).unwrap();
                let mut ids = Vec::with_capacity(functions.len() + 2);
                ids.push(n);
                ids.push(ci);
                for f in functions {
                    ids.push(self.compile_node(f));
                }
                let kids = self.push_children(&ids);
                let p = self.push_params(thresholds);
                self.push(
                    OpKind::IntervalSelect,
                    kids,
                    p,
                    u32::try_from(thresholds.len()).unwrap(),
                )
            }
            // The three point-evaluated leaves. Held as the original
            // `Density` and evaluated with `Density::compute`, because the
            // block-field evaluator does not recurse into them — see the
            // module doc.
            Density::Spline(_) => {
                let l = self.push_leaf(d);
                self.push(OpKind::Spline, l, 0, 0)
            }
            Density::Blended(_) => {
                let l = self.push_leaf(d);
                self.push(OpKind::Blended, l, 0, 0)
            }
            Density::FindTopSurface { .. } => {
                let l = self.push_leaf(d);
                self.push(OpKind::FindTopSurface, l, 0, 0)
            }
        }
    }

    /// Mirrors [`super::Field::eval`]'s traversal, tracking only the
    /// `interpolate` flag, to find which `interpolated` slots are reachable in
    /// an interpolating context. Kept next to `compile_node` so the two stay in
    /// step: an operator added there without a case here silently stops
    /// contributing its subtree to the walk.
    fn walk_interpolating(&self, id: NodeId, interpolate: bool, out: &mut Vec<u32>) {
        let op = self.op(id);
        match op.kind {
            OpKind::Interpolated => {
                if interpolate {
                    out.push(op.b);
                    // The inner is evaluated transparently, so anything below is
                    // *not* in an interpolating context.
                    self.walk_interpolating(op.a, false, out);
                } else {
                    self.walk_interpolating(op.a, false, out);
                }
            }
            // `flat_cache` also evaluates its inner with `interpolate = false`.
            OpKind::FlatCache => self.walk_interpolating(op.a, false, out),
            OpKind::Add | OpKind::Mul | OpKind::Min | OpKind::Max => {
                self.walk_interpolating(op.a, interpolate, out);
                self.walk_interpolating(op.b, interpolate, out);
            }
            OpKind::Abs
            | OpKind::Square
            | OpKind::Cube
            | OpKind::HalfNegative
            | OpKind::QuarterNegative
            | OpKind::Squeeze
            | OpKind::Invert
            | OpKind::Clamp
            | OpKind::Cache2D
            | OpKind::Marker => self.walk_interpolating(op.a, interpolate, out),
            OpKind::ShiftedNoise => {
                for i in 0..3 {
                    self.walk_interpolating(self.child(op.a + i), interpolate, out);
                }
            }
            OpKind::RangeChoice => {
                for i in 0..3 {
                    self.walk_interpolating(self.child(op.a + i), interpolate, out);
                }
            }
            OpKind::IntervalSelect => {
                let n = self.child(op.a);
                for i in 0..=n {
                    self.walk_interpolating(self.child(op.a + 1 + i), interpolate, out);
                }
            }
            // Leaves, and the point-evaluated leaves the field walk never
            // enters.
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
            | OpKind::FindTopSurface => {}
        }
    }

    fn unary(&mut self, kind: OpKind, a: &Density) -> NodeId {
        let child = self.compile_node(a);
        self.push(kind, child, 0, 0)
    }

    fn binary(&mut self, kind: OpKind, a: &Density, b: &Density) -> NodeId {
        let ca = self.compile_node(a);
        let cb = self.compile_node(b);
        self.push(kind, ca, cb, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(d: Density) -> Box<Density> {
        Box::new(d)
    }

    /// Every [`OpKind`] discriminant must equal the [`Density::kind_index`] of
    /// the variant it compiles from, so the `density_evals` per-kind counter
    /// reads the same bucket before and after the flattening.
    ///
    /// This is not decoration. A flattening pass that emitted `OpKind::Min`
    /// where the source said `max` would still *evaluate* — it would just
    /// evaluate the wrong operator, and the only visible symptom would be
    /// terrain. Pairing each kind with the `Density` variant it comes from
    /// makes the mapping assertable instead of a reading exercise.
    #[test]
    fn op_kind_discriminants_match_density_kind_index() {
        let cases: Vec<(OpKind, Density)> = vec![
            (OpKind::Const, Density::Const(0.0)),
            (OpKind::BlendAlpha, Density::BlendAlpha),
            (OpKind::BlendOffset, Density::BlendOffset),
            (OpKind::Beardifier, Density::Beardifier),
            (
                OpKind::YClampedGradient,
                Density::YClampedGradient {
                    from_y: 0.0,
                    to_y: 1.0,
                    from_value: 0.0,
                    to_value: 1.0,
                },
            ),
            (OpKind::Add, Density::Add(b(Density::Const(0.0)), b(Density::Const(0.0)))),
            (OpKind::Mul, Density::Mul(b(Density::Const(0.0)), b(Density::Const(0.0)))),
            (OpKind::Min, Density::Min(b(Density::Const(0.0)), b(Density::Const(0.0)))),
            (OpKind::Max, Density::Max(b(Density::Const(0.0)), b(Density::Const(0.0)))),
            (OpKind::Abs, Density::Abs(b(Density::Const(0.0)))),
            (OpKind::Square, Density::Square(b(Density::Const(0.0)))),
            (OpKind::Cube, Density::Cube(b(Density::Const(0.0)))),
            (OpKind::HalfNegative, Density::HalfNegative(b(Density::Const(0.0)))),
            (
                OpKind::QuarterNegative,
                Density::QuarterNegative(b(Density::Const(0.0))),
            ),
            (OpKind::Squeeze, Density::Squeeze(b(Density::Const(0.0)))),
            (OpKind::Invert, Density::Invert(b(Density::Const(0.0)))),
            (
                OpKind::Clamp,
                Density::Clamp {
                    input: b(Density::Const(0.0)),
                    min: 0.0,
                    max: 1.0,
                },
            ),
            (
                OpKind::Interpolated,
                Density::Interpolated {
                    inner: b(Density::Const(0.0)),
                    slot: 0,
                },
            ),
            (
                OpKind::FlatCache,
                Density::FlatCache {
                    inner: b(Density::Const(0.0)),
                    slot: 0,
                },
            ),
            (
                OpKind::Cache2D,
                Density::Cache2D {
                    inner: b(Density::Const(0.0)),
                    cache: Default::default(),
                },
            ),
            (OpKind::Marker, Density::Marker(b(Density::Const(0.0)))),
            (
                OpKind::RangeChoice,
                Density::RangeChoice {
                    input: b(Density::Const(0.0)),
                    min_inclusive: 0.0,
                    max_exclusive: 1.0,
                    when_in_range: b(Density::Const(0.0)),
                    when_out_of_range: b(Density::Const(0.0)),
                },
            ),
            (
                OpKind::IntervalSelect,
                Density::IntervalSelect {
                    input: b(Density::Const(0.0)),
                    thresholds: vec![0.0],
                    functions: vec![Density::Const(0.0), Density::Const(1.0)],
                },
            ),
            (
                OpKind::FindTopSurface,
                Density::FindTopSurface {
                    density: b(Density::Const(0.0)),
                    upper_bound: b(Density::Const(0.0)),
                    lower_bound: 0,
                    cell_height: 8,
                },
            ),
        ];

        for (kind, density) in &cases {
            assert_eq!(
                *kind as usize,
                density.kind_index(),
                "OpKind::{kind:?} = {} but Density::{}::kind_index() = {} — the \
                 flattening would file this node's counter under {:?}",
                *kind as usize,
                Density::KIND_NAMES[density.kind_index()],
                density.kind_index(),
                Density::KIND_NAMES[*kind as usize],
            );
            // And the compiled node must actually carry that kind, which is the
            // half of the mapping the discriminant equality above cannot see.
            let p = Program::compile(density);
            let root = p.graph().op(p.root());
            assert_eq!(
                root.kind, *kind,
                "compiling {} produced OpKind::{:?}",
                Density::KIND_NAMES[density.kind_index()],
                root.kind
            );
        }

        // Control on the control: the three noise-payload kinds and `Spline`
        // need a resolver to construct and are not in the list, so state the
        // coverage rather than letting silence imply completeness.
        assert_eq!(
            cases.len(),
            24,
            "the case list changed size; 24 of the 31 kinds are constructible \
             without a resolver (the 7 needing a NormalNoise/BlendedNoise/Spline \
             payload are covered by `compiles_the_real_router` in \
             tests/engine_semantics.rs against real data)"
        );
    }

    /// The width argument for flattening, measured rather than asserted in
    /// prose. `Density` inlines a `BlendedNoise` in one variant, so *every*
    /// node pays that width and every child is a separate allocation of it.
    #[test]
    fn density_node_is_much_wider_than_an_op() {
        let d = std::mem::size_of::<Density>();
        let o = std::mem::size_of::<Op>();
        assert!(
            o <= 16,
            "Op grew to {o} bytes; it is supposed to be a small fixed-width \
             record with wide payloads in side tables"
        );
        assert!(
            d >= 8 * o,
            "Density is {d} bytes and Op is {o}: the ratio ({}x) no longer \
             justifies the flattening's premise. If Density genuinely got \
             narrower that is good news, but this test's claim needs rewriting \
             rather than relaxing.",
            d / o
        );
        // Printed so the figure quoted in `docs/worldgen-density-engine.md` has a
        // source that is re-measured on every run, rather than a number someone
        // once computed by adding up struct fields.
        println!(
            "node width: size_of::<Density>() = {d}, size_of::<Op>() = {o}, ratio {}x",
            d / o
        );
    }

    /// Children must compile to lower ids than their parent, and the root must
    /// be the last node. The evaluator does not depend on this, but a
    /// compile pass that returned a stale id would otherwise be caught only by
    /// a value mismatch somewhere deep in a chunk.
    #[test]
    fn compilation_is_post_order_with_the_root_last() {
        let d = Density::Add(
            b(Density::Mul(b(Density::Const(2.0)), b(Density::Const(3.0)))),
            b(Density::Abs(b(Density::Const(-4.0)))),
        );
        let p = Program::compile(&d);
        assert_eq!(p.node_count(), 6, "2 consts + mul + const + abs + add");
        assert_eq!(p.root(), 5, "the root is the last node pushed");
        let g = p.graph();
        for id in 0..p.node_count() as u32 {
            let op = g.op(id);
            match op.kind {
                OpKind::Add | OpKind::Mul => {
                    assert!(op.a < id && op.b < id, "node {id} refers forward");
                }
                OpKind::Abs => assert!(op.a < id, "node {id} refers forward"),
                _ => {}
            }
        }
    }
}
