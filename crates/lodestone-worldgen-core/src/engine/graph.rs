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
//! `spline`, `old_blended_noise`, `find_top_surface` and `end_islands` are
//! **leaves** to the block-field evaluator: it does not recurse into them, it
//! calls the *point* interpreter ([`Density::compute`]). Everything beneath one of those is
//! therefore evaluated with point semantics — no quart snapping, no
//! interpolation. That is a real semantic, not an optimisation
//! (`docs/worldgen-density-engine.md`), so those four kinds compile to an
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

use std::collections::HashMap;
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
    EndIslands = 31,
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
    /// `spline` / `old_blended_noise` / `find_top_surface` / `end_islands`
    /// subtrees, held as the original `Density` because the block-field evaluator
    /// treats them as opaque point-evaluated leaves (module doc).
    leaves: Vec<Density>,
    /// Compile-time-only node-sharing tables. Emptied (and its allocations
    /// dropped) by [`Program::compile`] the moment compilation finishes, so a
    /// live `Graph` carries five empty `HashMap`s — see [`Interner`].
    interner: Interner,
}

/// The node-sharing (common-subexpression-elimination) tables, live only while
/// [`Program::compile`] runs.
///
/// # What it is for
///
/// [`Builder::build`](crate::density::Builder::build) resolves every `"minecraft:…"`
/// reference by *re-parsing* the referenced document, so vanilla's shared
/// density-function **DAG** arrives here as a **tree**: the same subtree appears
/// once per reference to it. §12.132 measured the consequence —
/// [`Program::cache_2d_under_leaves`] reading **708** against the handful of
/// `cache_2d` nodes 26.2's data declares — and identified it as the largest
/// remaining serial item in chunk generation. This restores the sharing.
///
/// # Why it is safe, and where the safety comes from
///
/// **Every RNG draw has already happened.** Noises are instantiated by
/// `Builder`, whose only sources are `master.from_hash_of(id)` (a *positional*
/// factory keyed by the noise's registry id) and `LegacyRandomSource::new(seed + n)`
/// — neither of which advances any shared stream. Compilation runs strictly
/// *after* every one of those draws and performs none of its own, so RNG draw
/// order and count, which are the specification of the generated world, are
/// unchanged by construction rather than by argument. Sharing at `Builder` level
/// would need that argument; sharing here does not, which is why the pass lives
/// on this side of the boundary.
///
/// **Every node kind is a pure function of position.** [`Graph`] holds no
/// mutable state at all (that is its module doc's point 3 — all memoisation is
/// in [`super::Scratch`]), every evaluator entry point takes `&self`, and no
/// `Op` and no [`Density`] variant reads anything but its own payload and the
/// `(x, y, z)` it is handed. So collapsing two structurally identical nodes into
/// one cannot change a value, cannot change an order, and cannot skip a side
/// effect — there are none to skip. The exclusion list this pass would otherwise
/// carry is therefore empty, and
/// [`Density::write_signature`](crate::density::Density::write_signature)
/// documents what to do if a future leaf kind breaks that property.
///
/// # The one thing that is not purely structural
///
/// `interpolated` and `flat_cache` carry a `slot` — an index into the
/// evaluator's per-chunk memo, assigned by a running counter in `Builder` and so
/// *different* for every duplicated copy. The signature deliberately excludes it
/// (see [`Density::write_signature`](crate::density::Density::write_signature)),
/// which is the whole point: collapsing two copies onto the surviving node's slot
/// is what makes the second parent hit [`super::Scratch`]'s slot memo instead of
/// re-evaluating the subtree. Slots freed this way are simply never used;
/// `Builder::slot_count` still sizes the scratch, so nothing downstream needs to
/// know.
///
/// # How the keys work
///
/// Side tables are interned first, so by the time an `Op` is keyed, `a`/`b`/`c`
/// are either already-canonical child [`NodeId`]s or already-canonical
/// side-table offsets — which makes `(kind, a, b, c)` a **complete** structural
/// key with no recursion and no deep comparison. Noises and leaves are keyed by
/// their exact bit-level signature (`write_signature`), never by a hash alone:
/// `HashMap` compares the full key on a hash collision, so a collision costs a
/// comparison rather than the wrong noise.
#[derive(Default)]
struct Interner {
    /// Exact `f64`-bit runs → offset into [`Graph::params`].
    params: HashMap<Box<[u64]>, u32>,
    /// Exact child-id runs → offset into [`Graph::children`].
    children: HashMap<Box<[u32]>, u32>,
    /// [`NormalNoise::write_signature`] → index into [`Graph::noises`].
    noises: HashMap<Box<[u64]>, u32>,
    /// [`Density::write_signature`] → index into [`Graph::leaves`].
    leaves: HashMap<Box<[u64]>, u32>,
    /// `(kind, a, b, c)` → the canonical node with that shape. `b` is replaced by
    /// [`SLOT_WILDCARD`] for `interpolated`/`flat_cache`.
    ops: HashMap<(u8, u32, u32, u32), NodeId>,
    /// Nodes answered by an existing entry rather than pushed. Exposed through
    /// [`Program::shared_nodes`] so a gate can assert the pass ran at all: a pass
    /// that shared nothing would satisfy every value assertion.
    shared_ops: u32,
    /// Of those, how many were an `interpolated`/`flat_cache` whose slot was
    /// collapsed onto an earlier one — the subset that removes *evaluation*
    /// rather than only memory.
    collapsed_slots: u32,
    /// Duplicate `NormalNoise` instantiations answered from the table. Reported
    /// rather than derived because `noises.len()` after the pass cannot tell you
    /// what it was before it.
    shared_noises: u32,
    /// Duplicate point-evaluated leaf subtrees answered from the table.
    shared_leaves: u32,
}

/// Stands in for the `slot` payload in an `interpolated`/`flat_cache` op key, so
/// two copies differing only in slot hash to the same bucket. Not a real slot
/// index: it is used for *every* node of those two kinds, so it cannot alias one.
const SLOT_WILDCARD: u32 = u32::MAX;

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
    /// Compilation is a **node-sharing** pass: an identical subtree is compiled
    /// once no matter how many times `Builder`'s reference expansion duplicated
    /// it. See [`Interner`] for why that is value- and RNG-invariant, and
    /// [`shared_nodes`](Self::shared_nodes) for the counter that proves it ran.
    #[must_use]
    pub fn compile(root: &Density) -> Self {
        let mut g = Graph {
            ops: Vec::new(),
            params: Vec::new(),
            children: Vec::new(),
            noises: Vec::new(),
            leaves: Vec::new(),
            interner: Interner::default(),
        };
        let id = g.compile_node(root);
        // The tables are only useful while compiling, and they are large (a leaf
        // signature includes every octave's 256-byte permutation table). Dropping
        // them here is most of the point of interning in the first place: the
        // whole `Graph` is meant to be small enough to stay cache-resident across
        // the 1,225 corner evaluations of a chunk.
        g.interner.params = HashMap::new();
        g.interner.children = HashMap::new();
        g.interner.noises = HashMap::new();
        g.interner.leaves = HashMap::new();
        g.interner.ops = HashMap::new();
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
    /// `find_top_surface`, `end_islands`). Exposed for the same reason as
    /// [`node_count`](Self::node_count): a gate needs to be able to see that
    /// the leaf boundary exists where the semantics say it does.
    #[must_use]
    pub fn leaf_count(&self) -> usize {
        self.graph.leaves.len()
    }

    /// Distinct instantiated noises in the shared graph.
    ///
    /// Before the node-sharing pass this equalled the number of `noise`/`shift*`/
    /// `shifted_noise` occurrences in the expanded tree, so one noise id
    /// referenced from ten places held ten full copies of its octave permutation
    /// tables. It now counts *distinct* noises, which is the quantity the noise
    /// kernel's cache footprint is proportional to.
    #[must_use]
    pub fn noise_count(&self) -> usize {
        self.graph.noises.len()
    }

    /// How many nodes the node-sharing pass answered from an existing entry
    /// instead of emitting.
    ///
    /// This is the pass's own "did it run" control, and it needs to be a counter
    /// rather than a value assertion for the reason §12.132's `Cache2D` deletion
    /// records: a sharing pass that shares *nothing* leaves every generated byte
    /// identical, so no terrain gate and no parity dump can distinguish it from a
    /// working one. Pair it with [`node_count`](Self::node_count): shared + kept
    /// is the size of the tree `Builder` handed over.
    #[must_use]
    pub fn shared_nodes(&self) -> usize {
        self.graph.interner.shared_ops as usize
    }

    /// Of [`shared_nodes`](Self::shared_nodes), how many were an
    /// `interpolated`/`flat_cache` collapsed onto an *earlier* node's cache slot.
    ///
    /// This is the subset that removes evaluation rather than only memory: the
    /// second parent of a collapsed node now hits [`super::Scratch`]'s slot memo
    /// where it used to re-evaluate the whole subtree beneath it. A pass with a
    /// large `shared_nodes` and a zero here would have made the graph smaller and
    /// the work identical.
    #[must_use]
    pub fn collapsed_slots(&self) -> usize {
        self.graph.interner.collapsed_slots as usize
    }

    /// Duplicate noise instantiations the pass collapsed. `noise_count() +
    /// shared_noises()` is how many copies `Builder` handed over.
    #[must_use]
    pub fn shared_noises(&self) -> usize {
        self.graph.interner.shared_noises as usize
    }

    /// Duplicate point-evaluated leaf subtrees the pass collapsed. `leaf_count() +
    /// shared_leaves()` is how many copies `Builder` handed over.
    #[must_use]
    pub fn shared_leaves(&self) -> usize {
        self.graph.interner.shared_leaves as usize
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
    /// Load-bearing for the sharing decision, and the count that convicted it.
    /// A [`Density::Cache2D`] used to carry a `Mutex`-backed last-value slot, so
    /// `Arc`-sharing one graph across threads turned a per-chunk cold cache into
    /// **708 slots contended by every generating worker**. §12.132 measured the
    /// consequence — instructions flat, IPC 5.46 → 1.32 at a window of 20 — and
    /// the memo is gone; the node is transparent in both evaluators.
    ///
    /// The count is still worth having, because it now measures something else:
    /// how many *duplicated expansions* of vanilla's shared `cache_2d` nodes sit
    /// under the leaves. 708 against vanilla's handful is the compiler expanding
    /// a DAG into a tree, which is why the memo could never hit — each parent got
    /// its own copy, so no two parents ever asked one slot for the same `(x, z)`.
    ///
    /// **Since [`Interner`] this reads 236 for the real `final_density`, and the
    /// residual is the interesting part.** Exactly 708 / 3: the leaf *table* held
    /// three copies of each `cache_2d`-bearing subtree and now holds one. The 236
    /// that remain are duplication *inside* a single leaf, which an `Op`-level
    /// pass structurally cannot see — a leaf is an untouched [`Density`] subtree
    /// evaluated by the point interpreter, so the pass interns it whole or not at
    /// all. `preliminary_surface_level` is the extreme case: **one** op, **one**
    /// leaf, **416** `cache_2d` nodes inside it. Removing that needs sharing in
    /// the `Density` tree itself (`Box` → `Arc` plus hash-consing in `Builder`),
    /// which is a different unit and carries the RNG-order argument this pass
    /// deliberately avoids needing. See `docs/worldgen-density-engine.md` and
    /// DESIGN.md §12.134.
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
        | Density::Blended(_)
        | Density::EndIslands(_) => Vec::new(),
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

    /// Emits a node, or returns the existing node with the same shape.
    ///
    /// `a`/`b`/`c` are already canonical when this is reached (children are
    /// compiled first; side-table payloads are interned first), so the key needs
    /// no recursion — see [`Interner`]'s *How the keys work*.
    fn push(&mut self, kind: OpKind, a: u32, b: u32, c: u32) -> NodeId {
        // `interpolated`/`flat_cache` key on their child alone: the `slot` in `b`
        // is a memo index, not part of the function, and collapsing it is the
        // point of the pass.
        let slotted = matches!(kind, OpKind::Interpolated | OpKind::FlatCache);
        let key = (kind as u8, a, if slotted { SLOT_WILDCARD } else { b }, c);
        if let Some(&existing) = self.interner.ops.get(&key) {
            self.interner.shared_ops += 1;
            if slotted && self.ops[existing as usize].b != b {
                self.interner.collapsed_slots += 1;
            }
            return existing;
        }
        let id = self.ops.len() as NodeId;
        self.ops.push(Op { kind, a, b, c });
        self.interner.ops.insert(key, id);
        id
    }

    /// Interns an exact `f64` run. Only whole-run matches count — deliberately no
    /// suffix/overlap sharing, because every read is `offset + known arity` and a
    /// partial match would need the arity to be part of the key.
    fn push_params(&mut self, vals: &[f64]) -> u32 {
        let key: Box<[u64]> = vals.iter().map(|v| v.to_bits()).collect();
        if let Some(&at) = self.interner.params.get(&key) {
            return at;
        }
        let at = self.params.len() as u32;
        self.params.extend_from_slice(vals);
        self.interner.params.insert(key, at);
        at
    }

    fn push_children(&mut self, ids: &[NodeId]) -> u32 {
        let key: Box<[u32]> = ids.into();
        if let Some(&at) = self.interner.children.get(&key) {
            return at;
        }
        let at = self.children.len() as u32;
        self.children.extend_from_slice(ids);
        self.interner.children.insert(key, at);
        at
    }

    fn push_noise(&mut self, n: &NormalNoise) -> u32 {
        let mut sig = Vec::new();
        n.write_signature(&mut sig);
        let key: Box<[u64]> = sig.into();
        if let Some(&at) = self.interner.noises.get(&key) {
            self.interner.shared_noises += 1;
            return at;
        }
        let at = self.noises.len() as u32;
        self.noises.push(n.clone());
        self.interner.noises.insert(key, at);
        at
    }

    fn push_leaf(&mut self, d: &Density) -> u32 {
        let mut sig = Vec::new();
        d.write_signature(&mut sig);
        let key: Box<[u64]> = sig.into();
        if let Some(&at) = self.interner.leaves.get(&key) {
            self.interner.shared_leaves += 1;
            return at;
        }
        let at = self.leaves.len() as u32;
        self.leaves.push(d.clone());
        self.interner.leaves.insert(key, at);
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
    /// | `Spline`/`Blended`/`FindTopSurface`/`EndIslands` | leaf idx | — | — |
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
            Density::FlatCache { inner, slot, .. } => {
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
            // A fourth point-evaluated leaf. It has no children to flatten (a
            // vanilla `SimpleFunction`) and it is xz-only, so nothing beneath it
            // could gain block-field semantics anyway — the leaf table is both
            // the simplest and the correct home. Interning it by signature is
            // what makes the End's two occurrences share one 256-byte
            // permutation instead of two.
            Density::EndIslands(_) => {
                let l = self.push_leaf(d);
                self.push(OpKind::EndIslands, l, 0, 0)
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
            | OpKind::FindTopSurface
            | OpKind::EndIslands => {}
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
                    memo: crate::density::memo_id_for(&Density::Const(0.0)),
                },
            ),
            (
                OpKind::Cache2D,
                Density::Cache2D {
                    inner: b(Density::Const(0.0)),
                    memo: crate::density::memo_id_for(&Density::Const(0.0)),
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
            // The real payload rather than a stand-in, because this pairing is
            // the *only* thing that catches `OpKind` and `kind_index` disagreeing
            // about `end_islands`, and a case omitted from this list is a case the
            // gate does not cover. ~17,500 RNG draws, microseconds.
            (
                OpKind::EndIslands,
                Density::EndIslands(Arc::new(crate::noise::EndIslandNoise::new(0))),
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
        // coverage rather than letting silence imply completeness. `EndIslands`
        // used to be in that group and no longer is — its payload is seeded from a
        // bare `i64`, so it is constructible here and is covered exactly.
        assert_eq!(
            cases.len(),
            25,
            "the case list changed size; 25 of the 32 kinds are constructible \
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

    /// One `NormalNoise` instantiated twice from the same seed and id — what
    /// `Builder` produces for every reference to a shared density function. Two
    /// separate objects, bit-identical contents.
    fn twin_noises() -> (NormalNoise, NormalNoise) {
        use crate::rng::{Algorithm, PositionalRandomFactory};
        let master = Algorithm::Xoroshiro.root_positional(42);
        let amps = [1.0, 1.0, 1.0];
        let mut a = master.from_hash_of("minecraft:continentalness");
        let first = NormalNoise::create(&mut a, -9, &amps);
        let mut b = master.from_hash_of("minecraft:continentalness");
        let second = NormalNoise::create(&mut b, -9, &amps);
        (first, second)
    }

    /// The node-sharing pass, on the shape `Builder`'s reference expansion
    /// actually produces: the same subtree twice, as two independent objects.
    ///
    /// The prediction is computed, not observed. The subtree
    /// `add(abs(noise), const)` is 4 nodes; `add(sub, sub)` over two *separate
    /// but identical* copies is 9 nodes as a tree (4 + 4 + 1) and 5 as a DAG
    /// (4 + 1), so `shared_nodes` must be exactly 4 and `node_count` exactly 5.
    /// Asserting only "fewer than 9" would pass for a pass that shared one node
    /// out of four — the *magnitude* species of vacuous test.
    #[test]
    fn an_identical_subtree_is_compiled_once() {
        let (n1, n2) = twin_noises();
        let sub = |n: NormalNoise| {
            Density::Add(
                b(Density::Abs(b(Density::Noise {
                    noise: n,
                    xz_scale: 0.25,
                    y_scale: 0.0,
                }))),
                b(Density::Const(1.5)),
            )
        };
        let d = Density::Add(b(sub(n1)), b(sub(n2)));
        let p = Program::compile(&d);
        assert_eq!(p.node_count(), 5, "noise + abs + const + inner add + outer add");
        assert_eq!(p.shared_nodes(), 4, "the second copy's four nodes");
        assert_eq!(p.noise_count(), 1, "two instantiations, one stored noise");
        assert_eq!(p.shared_noises(), 1);

        // …and the value is unchanged. `Density::compute` is the independent
        // arm: it walks the original *tree*, so it never sees the sharing.
        let mut scratch = super::super::Scratch::acquire(0, 4, 8, None);
        for (x, y, z) in [(0, 0, 0), (13, -37, 91), (-5, 200, 7)] {
            let flat =
                super::super::Field::new(p.graph(), super::super::Geom { cell_width: 4, cell_height: 8 }, &mut scratch)
                    .eval(p.root(), x, y, z, true);
            let tree = d.compute(crate::density::Context::new(x, y, z));
            assert_eq!(flat.to_bits(), tree.to_bits(), "at ({x}, {y}, {z})");
        }
        scratch.release();
    }

    /// `0.0` and `-0.0` compare *equal* under `==` and are different values
    /// under `1.0 / x`, so a signature built from compared floats rather than
    /// raw bits would fuse them and silently change terrain. Predict 2 nodes,
    /// not "at least 1".
    #[test]
    fn signed_zero_constants_do_not_share_a_node() {
        let d = Density::Add(b(Density::Const(0.0)), b(Density::Const(-0.0)));
        let p = Program::compile(&d);
        assert_eq!(p.node_count(), 3, "two distinct consts plus the add");
        assert_eq!(p.shared_nodes(), 0);

        // The control: two constants that really are the same value do share,
        // so the assertion above is about signed zero and not about the pass
        // being switched off.
        let same = Density::Add(b(Density::Const(0.0)), b(Density::Const(0.0)));
        let q = Program::compile(&same);
        assert_eq!(q.node_count(), 2, "one const plus the add");
        assert_eq!(q.shared_nodes(), 1);
    }

    /// The slot collapse: two `flat_cache` nodes over one inner, carrying the
    /// *different* slot indices `Builder`'s running counter would have given
    /// them. They must fuse onto the first slot — this is the part of the pass
    /// that removes evaluation rather than only memory, because the second
    /// parent now reads [`super::Scratch`]'s slot memo.
    #[test]
    fn duplicate_flat_cache_slots_collapse_onto_one() {
        let (n1, n2) = twin_noises();
        let inner = |n: NormalNoise| {
            Density::Noise {
                noise: n,
                xz_scale: 1.0,
                y_scale: 0.0,
            }
        };
        let d = Density::Add(
            // Distinct `memo` ids as well as distinct slots, so this doubles as
            // the gate that `write_signature` excludes the memo id: if it did
            // not, the two nodes would no longer collapse.
            b(Density::FlatCache {
                memo: crate::density::memo_id_for(&inner(n1.clone())),
                inner: b(inner(n1)),
                slot: 0,
            }),
            b(Density::FlatCache {
                memo: crate::density::memo_id_for(&inner(n2.clone())),
                inner: b(inner(n2)),
                slot: 1,
            }),
        );
        let p = Program::compile(&d);
        assert_eq!(p.node_count(), 3, "noise + one flat_cache + add");
        assert_eq!(p.collapsed_slots(), 1, "slot 1 folded onto slot 0");
        assert_eq!(
            p.interpolating_slots(),
            Vec::<u32>::new(),
            "flat_cache is not an interpolating slot"
        );

        // Value identity against the tree walker, which has no slots at all.
        let mut scratch = super::super::Scratch::acquire(2, 4, 8, None);
        for (x, y, z) in [(0, 0, 0), (7, 44, -19)] {
            let flat = super::super::Field::new(
                p.graph(),
                super::super::Geom { cell_width: 4, cell_height: 8 },
                &mut scratch,
            )
            .eval(p.root(), x, y, z, true);
            // `flat_cache` snaps XZ to the quart grid and forces y = 0, so the
            // expectation is the tree walker at the *snapped* position — the
            // semantic the collapse must not disturb.
            let (qx, qz) = ((x >> 2) << 2, (z >> 2) << 2);
            let one = Density::Noise {
                noise: twin_noises().0,
                xz_scale: 1.0,
                y_scale: 0.0,
            }
            .compute(crate::density::Context::new(qx, 0, qz));
            assert_eq!(flat.to_bits(), (one + one).to_bits(), "at ({x}, {y}, {z})");
        }
        scratch.release();
    }

    /// Two noises that differ must not share, or the pass would be handing one
    /// channel another channel's field. The control for
    /// [`an_identical_subtree_is_compiled_once`]: same shape, different data,
    /// opposite verdict.
    #[test]
    fn different_noises_do_not_share_a_table_entry() {
        use crate::rng::{Algorithm, PositionalRandomFactory};
        let master = Algorithm::Xoroshiro.root_positional(42);
        let amps = [1.0, 1.0, 1.0];
        let mut a = master.from_hash_of("minecraft:continentalness");
        let mut b_src = master.from_hash_of("minecraft:erosion");
        let d = Density::Add(
            b(Density::Noise {
                noise: NormalNoise::create(&mut a, -9, &amps),
                xz_scale: 1.0,
                y_scale: 1.0,
            }),
            b(Density::Noise {
                noise: NormalNoise::create(&mut b_src, -9, &amps),
                xz_scale: 1.0,
                y_scale: 1.0,
            }),
        );
        let p = Program::compile(&d);
        assert_eq!(p.noise_count(), 2, "different ids, different fields");
        assert_eq!(p.shared_noises(), 0);
        assert_eq!(p.node_count(), 3);
    }

    /// The End's `end_islands` leaf, which appears **twice** in 26.2's data
    /// (`noise_settings/end.json`'s `erosion` and `end/sloped_cheese.json`), and
    /// the two things that must be true of the pair.
    ///
    /// The subject is deliberately **two separately constructed**
    /// `EndIslandNoise`s rather than two clones of one `Arc`: that is the strong
    /// form. `Builder` shares an `Arc` so the ~17,500 draws are paid once, but if
    /// this pass only deduped by pointer it would silently stop working the moment
    /// anything built the leaf twice. Interning by signature covers both.
    ///
    /// What this does **not** claim: sharing the leaf does not make the End
    /// evaluate `end_islands` once per `(x, z)`. The point interpreter has no
    /// per-node memo and `cache_2d` is transparent (§12.132), so both occurrences
    /// still *evaluate*; what is shared is the compiled node and its 256-byte
    /// permutation. §12.134 records the same distinction for the overworld.
    #[test]
    fn the_two_end_islands_occurrences_share_one_leaf() {
        let d = Density::Add(
            b(Density::Cache2D {
                inner: b(Density::EndIslands(Arc::new(
                    crate::noise::EndIslandNoise::new(42),
                ))),
                memo: crate::density::XzMemoId::NONE,
            }),
            b(Density::EndIslands(Arc::new(
                crate::noise::EndIslandNoise::new(42),
            ))),
        );
        let p = Program::compile(&d);
        assert_eq!(p.leaf_count(), 1, "one leaf for both occurrences");
        assert_eq!(p.shared_leaves(), 1, "the second occurrence was interned");
        // ops: end_islands + cache_2d + add. The bare occurrence shares the
        // end_islands node with the one under cache_2d.
        assert_eq!(p.node_count(), 3);
        assert_eq!(p.shared_nodes(), 1);

        // A different seed must not share — the control that says the assertion
        // above is about identity and not about the table swallowing everything.
        let other = Density::Add(
            b(Density::EndIslands(Arc::new(
                crate::noise::EndIslandNoise::new(42),
            ))),
            b(Density::EndIslands(Arc::new(
                crate::noise::EndIslandNoise::new(43),
            ))),
        );
        let q = Program::compile(&other);
        assert_eq!(q.leaf_count(), 2, "different seeds, different fields");
        assert_eq!(q.shared_leaves(), 0);
    }

    /// `IntervalSelect` stores `n` as `children[a]` and its thresholds in
    /// `params`, so interning those runs is what makes two identical copies
    /// key alike. Without run interning the child offsets differ and the op key
    /// never matches — a silently ineffective pass on exactly the node kind
    /// whose payload is widest.
    #[test]
    fn wide_payload_nodes_share_through_their_interned_runs() {
        let arm = || Density::IntervalSelect {
            input: b(Density::Const(0.5)),
            thresholds: vec![0.0, 1.0],
            functions: vec![Density::Const(1.0), Density::Const(2.0), Density::Const(3.0)],
        };
        let d = Density::Add(b(arm()), b(arm()));
        let p = Program::compile(&d);
        // Tree: 2 x (input const + 3 function consts + the select) = 10, + add.
        // DAG: 4 distinct consts + 1 select + add = 6.
        assert_eq!(p.node_count(), 6, "4 consts, 1 interval_select, 1 add");
        assert_eq!(p.shared_nodes(), 5, "the whole second arm");
        let mut scratch = super::super::Scratch::acquire(0, 4, 8, None);
        let flat = super::super::Field::new(
            p.graph(),
            super::super::Geom { cell_width: 4, cell_height: 8 },
            &mut scratch,
        )
        .eval(p.root(), 1, 2, 3, true);
        assert_eq!(
            flat.to_bits(),
            d.compute(crate::density::Context::new(1, 2, 3)).to_bits()
        );
        scratch.release();
    }
}
