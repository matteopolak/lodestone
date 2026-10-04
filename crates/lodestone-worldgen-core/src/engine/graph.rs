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
//! The compiler also removes constant-only arithmetic and selectors before
//! emitting their children. This is a conservative field pass: an
//! `interpolated` or `flat_cache` wrapper is never folded because its cache
//! write is observable, while a selector with a constant input can discard the
//! branch that the evaluator could never enter.
//!
//! # What it deliberately does *not* flatten
//!
//! `old_blended_noise`, `find_top_surface` and `end_islands` remain opaque
//! leaves to the block-field evaluator. A `spline` is still a point-semantics
//! boundary, but its control points and nested values are compiled into the
//! indexed [`PointProgram`] side table instead of retaining a boxed recursive
//! walk. Everything beneath a spline is therefore still evaluated with point
//! semantics — no quart snapping or interpolation — while the compiled spline
//! program avoids the source enum's recursive payload walk.
//!
//! # Node kind fidelity
//!
//! Source operators retain [`Density::kind_index`]'s discriminants. Internal
//! compiled tags use [`OpKind::density_kind`] to retain the source counter bucket.
//! The two transparent wrapper indexes are intentionally absent from the field graph.
//! [`tests::op_kind_discriminants_match_density_kind_index`] is that gate — and
//! it is the gate that would catch a flattening pass mislabelling a node,
//! which is otherwise invisible (a mislabelled node still *evaluates*, it just
//! evaluates as the wrong operator).

//! # Constant-folding boundary
//!
//! [`constant_value`] is intentionally a whitelist. It may grow only for
//! operations whose result and evaluation side effects are both determined by
//! their constant children. In particular, do not fold an entered sampler
//! wrapper or a subtree whose cache write can be reached: later queries rely on
//! that slot being populated in the same order. An unreachable right operand of
//! a zero-first `mul` is the explicit short-circuit exception.

use std::collections::HashMap;
use std::sync::Arc;

use super::point::PointProgram;
use super::xz_products::{XzProductIdentity, XzProductKind, XzProductManifest};
use crate::density::{Density, Spline};
use crate::noise::NormalNoise;

/// An index into [`Graph::ops`].
pub type NodeId = u32;


/// The operator of one flattened node.
///
/// Source operators use the matching [`Density::kind_index`] discriminant.
/// Indexes 19 and 20 are reserved for transparent wrappers omitted from the
/// field graph. Internal compiled tags map back through [`Self::density_kind`].
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
    // 19 and 20 are the transparent Cache2D and Marker density kinds.
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
    DeepTerrainRangeChoice = 32,
}

impl OpKind {
    #[inline(always)]
    pub(crate) fn density_kind(self) -> usize {
        match self {
            Self::DeepTerrainRangeChoice => Self::RangeChoice as usize,
            _ => self as usize,
        }
    }
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

/// Marks a clamp whose max child may evaluate its right side first.
///
/// This is set only after interning, once [`Graph::compute_tile_eligibility`]
/// has proved the max left side has no cache effects. `Clamp` otherwise leaves
/// `Op::c` at zero.
const CLAMP_MAX_RHS_FIRST: u32 = u32::MAX;

#[derive(Clone, Copy, Debug)]
pub(crate) struct TileOp {
    pub(crate) kind: OpKind,
    pub(crate) a: u16,
    pub(crate) b: u16,
    pub(crate) c: u16,
    pub(crate) params: u32,
    pub(crate) aux: u32,
    pub(crate) branch_count: u16,
    pub(crate) threshold_count: u16,
    pub(crate) product: u8,
    pub(crate) source: NodeId,
}

#[derive(Clone, Debug)]
pub(crate) struct TilePlan {
    pub(crate) ops: Vec<TileOp>,
    pub(crate) params: Vec<f64>,
    pub(crate) branches: Vec<u16>,
    pub(crate) root: u16,
    pub(crate) roots: Vec<u16>,
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
    /// `old_blended_noise` / `find_top_surface` / `end_islands` subtrees, held
    /// as original `Density` values because the block-field evaluator treats
    /// them as opaque point-evaluated leaves.
    leaves: Vec<Density>,
    /// Compiled point-semantics spline programs. The field evaluator enters one
    /// of these without calling the source `Spline::compute` walk.
    splines: Vec<PointProgram>,
    /// Compile-time-only node-sharing tables. Emptied (and its allocations
    /// dropped) by [`Program::compile`] the moment compilation finishes, so a
    /// live `Graph` carries six empty `HashMap`s — see [`Interner`].
    interner: Interner,
    /// The exact production final-density shape, when this graph is eligible
    /// for the bounded cell evaluator.
    overworld_final_density: Option<OverworldFinalDensityPlan>,
    deep_terrain: Option<DeepTerrainPlan>,
    /// A shared pure plan for the terrain and four noodle roots. The roots are
    /// compiled together so their common register subtrees are evaluated once.
    overworld_final_density_tile_plan: Option<TilePlan>,
    /// Product labels are collected only during compilation. The compact
    /// per-node table is immutable after the graph is published.
    product_manifest: Option<XzProductManifest>,
    product_nodes: Vec<(NodeId, XzProductKind)>,
    product_kinds: Vec<Option<XzProductKind>>,
    product_identity: Option<XzProductIdentity>,
    product_fingerprint: Option<u64>,
    /// Nodes whose field evaluation has no sampler-cache side effects and can
    /// therefore be evaluated once for all eight corners of a cell.
    tile_eligible: Vec<bool>,
    tile_plan_ids: Vec<Option<u16>>,
    tile_plans: Vec<TilePlan>,
}

/// The five nodes needed by the production final-density cell plan.
///
/// The graph matcher fills this only for the complete root shape. Keeping the
/// child ids, rather than only the slots, preserves the exact point evaluator
/// beneath each interpolation boundary.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OverworldFinalDensityPlan {
    pub(crate) pre_corner_slope: Option<NodeId>,
    pub(crate) prepared_blended: Option<NodeId>,
    pub(crate) terrain_inner: NodeId,
    pub(crate) terrain_slot: usize,
    pub(crate) noodle_control_inner: NodeId,
    pub(crate) noodle_control_slot: usize,
    pub(crate) noodle_thickness_inner: NodeId,
    pub(crate) noodle_thickness_slot: usize,
    pub(crate) noodle_ridge_a_inner: NodeId,
    pub(crate) noodle_ridge_a_slot: usize,
    pub(crate) noodle_ridge_b_inner: NodeId,
    pub(crate) noodle_ridge_b_slot: usize,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DeepTerrainPlan {
    pub(crate) range: NodeId,
    pub(crate) non_blended: NodeId,
    pub(crate) blended: NodeId,
    pub(crate) residual: NodeId,
}

impl DeepTerrainPlan {
    pub(crate) fn admits(self, value: f64) -> bool {
        (4.345..=65536.0).contains(&value)
    }
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
    /// [`Spline::write_signature`] → index into [`Graph::splines`].
    splines: HashMap<Box<[u64]>, u32>,
    /// `(kind, a, b, c)` → the canonical node with that shape. `b` is replaced by
    /// [`SLOT_WILDCARD`] for `interpolated`/`flat_cache`.
    ops: HashMap<(u8, u32, u32, u32), NodeId>,
    /// Nodes answered by an existing entry rather than pushed. Exposed through
    /// [`Program::shared_nodes`] so a gate can assert the pass ran at all: a pass
    /// that shared nothing would satisfy every value assertion.
    shared_ops: u32,
    /// Composite nodes replaced by a side-effect-free constant before their
    /// children were emitted.
    constant_folds: u32,
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
    /// Duplicate spline programs answered from the compact side table.
    shared_splines: u32,
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
        Self::compile_inner(root, None, true)
    }

    /// Compiles a graph with an exact request-scoped X/Z product manifest.
    #[must_use]
    pub fn compile_with_xz_products(root: &Density, manifest: XzProductManifest) -> Self {
        Self::compile_inner(root, Some(manifest), false)
    }

    fn compile_inner(
        root: &Density,
        manifest: Option<XzProductManifest>,
        enable_tiles: bool,
    ) -> Self {
        let mut g = Graph {
            ops: Vec::new(),
            params: Vec::new(),
            children: Vec::new(),
            noises: Vec::new(),
            leaves: Vec::new(),
            splines: Vec::new(),
            interner: Interner::default(),
            overworld_final_density: None,
            deep_terrain: None,
            overworld_final_density_tile_plan: None,
            product_manifest: manifest,
            product_nodes: Vec::new(),
            product_kinds: Vec::new(),
            product_identity: None,
            product_fingerprint: None,
            tile_eligible: Vec::new(),
            tile_plan_ids: Vec::new(),
            tile_plans: Vec::new(),
        };
        let id = g.compile_node(root);
        let overworld_final_density = g.detect_overworld_final_density(id);
        g.overworld_final_density = overworld_final_density;
        g.deep_terrain = g.compile_deep_terrain_residual();
        if let Some(plan) = g.deep_terrain {
            g.ops[plan.range as usize].kind = OpKind::DeepTerrainRangeChoice;
        }
        let has_product_nodes = !g.product_nodes.is_empty();
        let manifest_identity = g
            .product_manifest
            .as_ref()
            .map(XzProductManifest::identity);
        let manifest_fingerprint = manifest_identity
            .as_ref()
            .map(XzProductIdentity::diagnostic_token);
        g.product_kinds.resize(g.ops.len(), None);
        for (node, kind) in g.product_nodes.drain(..) {
            g.product_kinds[node as usize] = Some(kind);
        }
        // A route compiled with the shared manifest is only lattice-admitted
        // when it actually contains a recognized product wrapper. This lets
        // final density use factor alone while preliminary surface level uses
        // the factor/offset pair, yet keeps a product-free route on its exact
        // ordinary path.
        g.product_identity = has_product_nodes.then_some(manifest_identity).flatten();
        g.product_fingerprint = has_product_nodes.then_some(manifest_fingerprint).flatten();
        let pure = g.compute_tile_eligibility(false);
        g.mark_clamp_max_rhs_first(&pure);
        if enable_tiles || g.product_manifest.is_some() {
            g.tile_eligible = pure.clone();
        } else {
            g.tile_eligible.resize(g.ops.len(), false);
        }
        g.tile_plan_ids.resize(g.ops.len(), None);
        if let Some(plan) = g.overworld_final_density {
            let final_roots = [
                plan.terrain_inner,
                plan.noodle_control_inner,
                plan.noodle_ridge_a_inner,
                plan.noodle_ridge_b_inner,
                plan.noodle_thickness_inner,
            ];
            if final_roots
                .iter()
                .all(|&root| g.tile_eligible[root as usize])
            {
                g.overworld_final_density_tile_plan = Some(g.compile_tile_plan_for_roots(&final_roots));
            }
            for root in [
                plan.terrain_inner,
                plan.noodle_control_inner,
                plan.noodle_thickness_inner,
                plan.noodle_ridge_a_inner,
                plan.noodle_ridge_b_inner,
            ] {
                if g.tile_plan_ids[root as usize].is_none() && g.tile_eligible[root as usize] {
                    let index = u16::try_from(g.tile_plans.len())
                        .expect("too many compiled tile plans");
                    let compiled = g.compile_tile_plan(root);
                    g.tile_plans.push(compiled);
                    g.tile_plan_ids[root as usize] = Some(index);
                }
            }
        }
        g.product_manifest = None;
        // The tables are only useful while compiling, and they are large (a leaf
        // signature includes every octave's 256-byte permutation table). Dropping
        // them here is most of the point of interning in the first place: the
        // whole `Graph` is meant to be small enough to stay cache-resident across
        // the 1,225 corner evaluations of a chunk.
        g.interner.params = HashMap::new();
        g.interner.children = HashMap::new();
        g.interner.noises = HashMap::new();
        g.interner.leaves = HashMap::new();
        g.interner.splines = HashMap::new();
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

    /// Returns the structural identity of the recognized X/Z product pair.
    #[must_use]
    pub fn xz_product_fingerprint(&self) -> Option<u64> {
        self.graph.product_fingerprint
    }

    /// Returns the complete structural identity used for product admission.
    #[must_use]
    pub fn xz_product_identity(&self) -> Option<XzProductIdentity> {
        self.graph.product_identity.clone()
    }

    /// Whether this program has the bounded production final-density shape.
    ///
    /// The public boolean keeps the graph's node ids private to the evaluator
    /// while allowing a production driver to choose its cell loop once.
    #[must_use]
    pub fn has_overworld_final_density_cell_plan(&self) -> bool {
        self.graph.overworld_final_density.is_some()
    }

    pub(crate) fn overworld_final_density_plan(&self) -> Option<OverworldFinalDensityPlan> {
        self.graph.overworld_final_density
    }

    /// Number of flattened nodes in the shared graph — an implementation
    /// detail exposed only so gates can assert the flattening actually
    /// happened (a graph of one node would pass every value assertion while
    /// having flattened nothing).
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.graph.ops.len()
    }

    /// Number of pure nodes eligible for eight-corner cell evaluation.
    /// Cache-bearing and point-boundary nodes remain on the scalar fallback.
    #[must_use]
    pub fn tile_eligible_node_count(&self) -> usize {
        self.graph.tile_eligible.iter().filter(|&&value| value).count()
    }

    #[must_use]
    pub fn tile_plan_count(&self) -> usize {
        self.graph.tile_plans.len()
    }


    /// Number of opaque point-evaluated leaves other than splines
    /// (`old_blended_noise`, `find_top_surface`, `end_islands`). Exposed for the same reason as
    /// [`node_count`](Self::node_count): a gate needs to be able to see that
    /// the leaf boundary exists where the semantics say it does.
    #[must_use]
    pub fn leaf_count(&self) -> usize {
        self.graph.leaves.len()
    }

    /// Number of compact compiled spline programs reachable from this graph.
    /// This is separate from [`Self::leaf_count`] because spline control data
    /// no longer retains a boxed source subtree.
    #[must_use]
    pub fn spline_count(&self) -> usize {
        self.graph.splines.len()
    }

    /// Duplicate spline payloads collapsed into one indexed program.
    #[must_use]
    pub fn shared_splines(&self) -> usize {
        self.graph.interner.shared_splines as usize
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
    /// working one. Pair it with [`node_count`](Self::node_count) and
    /// [`constant_folds`](Self::constant_folds): the latter also removes source
    /// nodes before the sharing table sees them.
    #[must_use]
    pub fn shared_nodes(&self) -> usize {
        self.graph.interner.shared_ops as usize
    }

    /// Composite nodes eliminated by constant folding during compilation.
    /// Direct literal nodes are not counted; this measures the structural pass,
    /// not the number of constants that remain in the graph.
    #[must_use]
    pub fn constant_folds(&self) -> usize {
        self.graph.interner.constant_folds as usize
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

    /// Duplicate opaque point-evaluated leaf subtrees other than splines that
    /// the pass collapsed. `leaf_count() + shared_leaves()` is how many copies
    /// `Builder` handed over for those leaf kinds.
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
            .filter(|op| Density::KIND_NAMES[op.kind.density_kind()] == kind_name)
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
    /// The count now covers only the remaining source-backed leaves. Spline
    /// payloads have their own indexed point programs, so their nested cache
    /// wrappers are intentionally absent from this census; inspect
    /// [`Program::spline_count`] for that separate representation.
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

/// Evaluates the side-effect-free constant subset of a density tree.
///
/// This is intentionally separate from [`Density::compute`]. It rejects every
/// sampler memo write that the field walk would enter, because removing one
/// would change the cache state observed by a later query even when its numeric
/// result is constant. A zero-first multiply does not inspect its right child,
/// so a memo below that unreachable branch is safe to omit.
fn constant_value(d: &Density) -> Option<f64> {
    match d {
        Density::Const(value) => Some(*value),
        Density::BlendAlpha => Some(1.0),
        Density::BlendOffset | Density::Beardifier => Some(0.0),
        Density::Add(a, b) => Some(constant_value(a)? + constant_value(b)?),
        Density::Mul(a, b) => {
            let first = constant_value(a)?;
            if first == 0.0 {
                // The field and point evaluators both return a positive zero
                // without entering the right operand in this case.
                Some(0.0)
            } else {
                Some(first * constant_value(b)?)
            }
        }
        Density::Min(a, b) => Some(constant_value(a)?.min(constant_value(b)?)),
        Density::Max(a, b) => Some(constant_value(a)?.max(constant_value(b)?)),
        Density::Abs(a) => Some(constant_value(a)?.abs()),
        Density::Square(a) => {
            let value = constant_value(a)?;
            Some(value * value)
        }
        Density::Cube(a) => {
            let value = constant_value(a)?;
            Some(value * value * value)
        }
        Density::HalfNegative(a) => {
            let value = constant_value(a)?;
            Some(if value > 0.0 { value } else { value * 0.5 })
        }
        Density::QuarterNegative(a) => {
            let value = constant_value(a)?;
            Some(if value > 0.0 { value } else { value * 0.25 })
        }
        Density::Squeeze(a) => {
            let value = crate::math::clamp(constant_value(a)?, -1.0, 1.0);
            Some(value / 2.0 - value * value * value / 24.0)
        }
        Density::Invert(a) => Some(1.0 / constant_value(a)?),
        Density::Clamp { input, min, max } => {
            Some(crate::math::clamp(constant_value(input)?, *min, *max))
        }
        // `marker` is transparent in both interpreters and has no cache of its
        // own. The three sampler wrappers below are deliberately absent: their
        // memo writes are observable even if their wrapped value is constant.
        Density::Marker(inner) => constant_value(inner),
        Density::RangeChoice {
            input,
            min_inclusive,
            max_exclusive,
            when_in_range,
            when_out_of_range,
        } => {
            let value = constant_value(input)?;
            if value >= *min_inclusive && value < *max_exclusive {
                constant_value(when_in_range)
            } else {
                constant_value(when_out_of_range)
            }
        }
        Density::IntervalSelect {
            input,
            thresholds,
            functions,
        } => {
            let value = constant_value(input)?;
            let index = thresholds
                .iter()
                .position(|threshold| value < *threshold)
                .unwrap_or(functions.len().checked_sub(1)?);
            functions.get(index).and_then(constant_value)
        }
        Density::YClampedGradient { .. }
        | Density::Interpolated { .. }
        | Density::FlatCache { .. }
        | Density::Cache2D { .. }
        | Density::Noise { .. }
        | Density::ShiftedNoise { .. }
        | Density::ShiftA(_)
        | Density::ShiftB(_)
        | Density::Shift(_)
        | Density::Spline(_)
        | Density::Blended(_)
        | Density::FindTopSurface { .. }
        | Density::EndIslands(_) => None,
    }
}

/// Whether a subtree can write a block-field cache when entered by
/// [`Field`](super::field::Field). Point-evaluated leaves stop the walk because
/// the field evaluator calls a point program or source point evaluator as one
/// opaque operation.
fn contains_field_cache_writer(d: &Density) -> bool {
    match d {
        Density::Interpolated { .. } | Density::FlatCache { .. } => true,
        Density::Cache2D { inner, .. } | Density::Marker(inner) => {
            contains_field_cache_writer(inner)
        }
        Density::Add(a, b) | Density::Mul(a, b) | Density::Min(a, b) | Density::Max(a, b) => {
            contains_field_cache_writer(a) || contains_field_cache_writer(b)
        }
        Density::Abs(a)
        | Density::Square(a)
        | Density::Cube(a)
        | Density::HalfNegative(a)
        | Density::QuarterNegative(a)
        | Density::Squeeze(a)
        | Density::Invert(a) => contains_field_cache_writer(a),
        Density::Clamp { input, .. } => contains_field_cache_writer(input),
        Density::ShiftedNoise {
            shift_x,
            shift_y,
            shift_z,
            ..
        } => {
            contains_field_cache_writer(shift_x)
                || contains_field_cache_writer(shift_y)
                || contains_field_cache_writer(shift_z)
        }
        Density::RangeChoice {
            input,
            when_in_range,
            when_out_of_range,
            ..
        } => {
            contains_field_cache_writer(input)
                || contains_field_cache_writer(when_in_range)
                || contains_field_cache_writer(when_out_of_range)
        }
        Density::IntervalSelect {
            input, functions, ..
        } => {
            contains_field_cache_writer(input)
                || functions.iter().any(contains_field_cache_writer)
        }
        Density::Const(_)
        | Density::BlendAlpha
        | Density::BlendOffset
        | Density::Beardifier
        | Density::YClampedGradient { .. }
        | Density::Noise { .. }
        | Density::ShiftA(_)
        | Density::ShiftB(_)
        | Density::Shift(_)
        | Density::Spline(_)
        | Density::Blended(_)
        | Density::FindTopSurface { .. }
        | Density::EndIslands(_) => false,
    }
}

impl Graph {
    pub(crate) fn ops(&self) -> &[Op] {
        &self.ops
    }

    pub(crate) fn params(&self) -> &[f64] {
        &self.params
    }

    pub(crate) fn children(&self) -> &[NodeId] {
        &self.children
    }

    pub(crate) fn noises(&self) -> &[NormalNoise] {
        &self.noises
    }

    pub(crate) fn leaves(&self) -> &[Density] {
        &self.leaves
    }

    pub(crate) fn splines(&self) -> &[PointProgram] {
        &self.splines
    }

    pub(crate) fn product_kind(&self, node: NodeId) -> Option<XzProductKind> {
        self.product_kinds.get(node as usize).copied().flatten()
    }

    pub(crate) fn product_identity(&self) -> Option<&XzProductIdentity> {
        self.product_identity.as_ref()
    }

    pub(crate) fn op(&self, id: NodeId) -> Op {
        self.ops[id as usize]
    }

    pub(crate) fn deep_terrain_plan(&self) -> Option<DeepTerrainPlan> {
        self.deep_terrain
    }

    pub(crate) fn child(&self, at: u32) -> NodeId {
        self.children[at as usize]
    }

    #[inline]
    pub(crate) fn tile_eligible(&self, id: NodeId) -> bool {
        self.tile_eligible[id as usize]
    }

    #[inline]
    pub(crate) fn clamp_max_rhs_first(&self, op: Op) -> Option<Op> {
        (op.kind == OpKind::Clamp && op.c == CLAMP_MAX_RHS_FIRST).then(|| self.op(op.a))
    }

    #[inline]
    pub(crate) fn tile_plan(&self, id: NodeId) -> Option<&TilePlan> {
        self.tile_plan_ids
            .get(id as usize)
            .and_then(|index| index.map(|index| &self.tile_plans[index as usize]))
    }

    pub(crate) fn overworld_final_density_tile_plan(&self) -> Option<&TilePlan> {
        self.overworld_final_density_tile_plan.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn compile_tile_plan_for_test(&self, root: NodeId) -> TilePlan {
        assert!(self.tile_eligible(root));
        self.compile_tile_plan(root)
    }

    fn compile_tile_plan(&self, root: NodeId) -> TilePlan {
        self.compile_tile_plan_for_roots(&[root])
    }

    fn compile_tile_plan_for_roots(&self, roots: &[NodeId]) -> TilePlan {
        fn visit(graph: &Graph, id: NodeId, plan: &mut TilePlan, ids: &mut [u16]) -> u16 {
            if ids[id as usize] != u16::MAX {
                return ids[id as usize];
            }
            let op = graph.op(id);
            let mut tile = TileOp {
                kind: op.kind,
                a: 0,
                b: 0,
                c: 0,
                params: 0,
                aux: 0,
                branch_count: 0,
                threshold_count: 0,
                product: match graph.product_kind(id) {
                    None => 0,
                    Some(XzProductKind::Factor) => 1,
                    Some(XzProductKind::Offset) => 2,
                },
                source: id,
            };
            match op.kind {
                OpKind::Add | OpKind::Mul | OpKind::Min | OpKind::Max => {
                    tile.a = visit(graph, op.a, plan, ids);
                    tile.b = visit(graph, op.b, plan, ids);
                }
                OpKind::Abs
                | OpKind::Square
                | OpKind::Cube
                | OpKind::HalfNegative
                | OpKind::QuarterNegative
                | OpKind::Squeeze
                | OpKind::Invert => {
                    tile.a = visit(graph, op.a, plan, ids);
                }
                OpKind::Clamp => {
                    tile.a = visit(graph, op.a, plan, ids);
                    tile.params = copy_params(graph, plan, op.b, 2);
                }
                OpKind::ShiftedNoise => {
                    tile.a = visit(graph, graph.child(op.a), plan, ids);
                    tile.b = visit(graph, graph.child(op.a + 1), plan, ids);
                    tile.c = visit(graph, graph.child(op.a + 2), plan, ids);
                    tile.params = copy_params(graph, plan, op.c, 2);
                    tile.aux = op.b;
                }
                OpKind::RangeChoice => {
                    tile.a = visit(graph, graph.child(op.a), plan, ids);
                    tile.b = visit(graph, graph.child(op.a + 1), plan, ids);
                    tile.c = visit(graph, graph.child(op.a + 2), plan, ids);
                    tile.params = copy_params(graph, plan, op.b, 2);
                }
                OpKind::IntervalSelect => {
                    let count = graph.child(op.a);
                    tile.a = visit(graph, graph.child(op.a + 1), plan, ids);
                    tile.params = copy_params(graph, plan, op.b, op.c);
                    let first = plan.branches.len();
                    for index in 0..count {
                        let branch = visit(
                            graph,
                            graph.child(op.a + 2 + index),
                            plan,
                            ids,
                        );
                        plan.branches.push(branch);
                    }
                    tile.aux = u32::try_from(first).expect("too many tile branches");
                    tile.branch_count = u16::try_from(count).expect("too many interval branches");
                    tile.threshold_count = u16::try_from(op.c).expect("too many interval thresholds");
                }
                OpKind::Const => tile.params = copy_params(graph, plan, op.a, 1),
                OpKind::YClampedGradient => tile.params = copy_params(graph, plan, op.a, 4),
                OpKind::Noise => {
                    tile.params = copy_params(graph, plan, op.b, 2);
                    tile.aux = op.a;
                }
                OpKind::ShiftA | OpKind::ShiftB | OpKind::Shift | OpKind::EndIslands => {
                    tile.aux = op.a;
                }
                OpKind::BlendAlpha
                | OpKind::BlendOffset
                | OpKind::Beardifier => {}
                OpKind::Interpolated
                | OpKind::FlatCache
                | OpKind::Spline
                | OpKind::Blended
                | OpKind::FindTopSurface
                | OpKind::DeepTerrainRangeChoice => {
                    unreachable!("cache-bearing node entered compiled tile plan")
                }
            }
            let reg = u16::try_from(plan.ops.len()).expect("tile plan exceeds register limit");
            ids[id as usize] = reg;
            plan.ops.push(tile);
            reg
        }

        fn copy_params(graph: &Graph, plan: &mut TilePlan, start: u32, count: u32) -> u32 {
            let at = u32::try_from(plan.params.len()).expect("too many tile parameters");
            plan.params.extend_from_slice(&graph.params[start as usize..][..count as usize]);
            at
        }

        let mut plan = TilePlan {
            ops: Vec::new(),
            params: Vec::new(),
            branches: Vec::new(),
            root: 0,
            roots: Vec::with_capacity(roots.len()),
        };
        let mut ids = vec![u16::MAX; self.ops.len()];
        for &root in roots {
            let compiled = visit(self, root, &mut plan, &mut ids);
            if plan.roots.is_empty() {
                plan.root = compiled;
            }
            plan.roots.push(compiled);
        }
        plan
    }


    fn compute_tile_eligibility(&self, allow_blended: bool) -> Vec<bool> {
        let mut eligible = vec![false; self.ops.len()];
        for (id, op) in self.ops.iter().copied().enumerate() {
            eligible[id] = match op.kind {
                OpKind::Const
                | OpKind::BlendAlpha
                | OpKind::BlendOffset
                | OpKind::Beardifier
                | OpKind::YClampedGradient
                | OpKind::Noise
                | OpKind::ShiftA
                | OpKind::ShiftB
                | OpKind::Shift
                | OpKind::EndIslands => true,
                OpKind::Blended => allow_blended,
                OpKind::Add | OpKind::Mul | OpKind::Min | OpKind::Max => {
                    eligible[op.a as usize] && eligible[op.b as usize]
                }
                OpKind::Abs
                | OpKind::Square
                | OpKind::Cube
                | OpKind::HalfNegative
                | OpKind::QuarterNegative
                | OpKind::Squeeze
                | OpKind::Invert
                | OpKind::Clamp => eligible[op.a as usize],
                OpKind::ShiftedNoise => {
                    eligible[self.child(op.a) as usize]
                        && eligible[self.child(op.a + 1) as usize]
                        && eligible[self.child(op.a + 2) as usize]
                }
                OpKind::RangeChoice => {
                    eligible[self.child(op.a) as usize]
                        && eligible[self.child(op.a + 1) as usize]
                        && eligible[self.child(op.a + 2) as usize]
                }
                OpKind::IntervalSelect => {
                    let n = self.child(op.a);
                    eligible[self.child(op.a + 1) as usize]
                        && (0..n).all(|index| eligible[self.child(op.a + 2 + index) as usize])
                }
                // These kinds either write a sampler cache or cross into a
                // separate evaluator whose cache state is not tile-local.
                OpKind::Interpolated
                | OpKind::FlatCache
                | OpKind::Spline
                | OpKind::FindTopSurface
                | OpKind::DeepTerrainRangeChoice => false,
            };
        }
        eligible
    }



    fn detect_overworld_final_density(&self, root: NodeId) -> Option<OverworldFinalDensityPlan> {
        let root_op = self.op(root);
        if root_op.kind != OpKind::Min {
            return None;
        }

        let squeeze = self.op(root_op.a);
        if squeeze.kind != OpKind::Squeeze {
            return None;
        }
        let terrain = self.op(squeeze.a);
        if terrain.kind != OpKind::Interpolated {
            return None;
        }

        let noodle = self.op(root_op.b);
        if noodle.kind != OpKind::RangeChoice
            || !self.params_equal(noodle.b, -1_000_000.0, 0.0)
        {
            return None;
        }
        let control = self.op(self.child(noodle.a));
        if control.kind != OpKind::Interpolated {
            return None;
        }
        if !self.const_equal(self.child(noodle.a + 1), 64.0) {
            return None;
        }

        let out = self.op(self.child(noodle.a + 2));
        if out.kind != OpKind::Add {
            return None;
        }
        let thickness = self.op(out.a);
        if thickness.kind != OpKind::Interpolated {
            return None;
        }

        let scaled_ridges = self.op(out.b);
        if scaled_ridges.kind != OpKind::Mul
            || !self.const_equal(scaled_ridges.a, 1.5)
        {
            return None;
        }
        let ridges = self.op(scaled_ridges.b);
        if ridges.kind != OpKind::Max {
            return None;
        }
        let ridge_a = self.op(ridges.a);
        let ridge_b = self.op(ridges.b);
        if ridge_a.kind != OpKind::Abs
            || ridge_b.kind != OpKind::Abs
            || self.op(ridge_a.a).kind != OpKind::Interpolated
            || self.op(ridge_b.a).kind != OpKind::Interpolated
        {
            return None;
        }

        let pre_corner_slope = self.detect_pre_corner_slope(terrain.a);
        let prepared_blended = pre_corner_slope.and_then(|_| {
            self.ops.iter().position(|op| op.kind == OpKind::Blended).map(|id| id as NodeId)
        });
        Some(OverworldFinalDensityPlan {
            pre_corner_slope,
            prepared_blended,
            terrain_inner: terrain.a,
            terrain_slot: terrain.b as usize,
            noodle_control_inner: control.a,
            noodle_control_slot: control.b as usize,
            noodle_thickness_inner: thickness.a,
            noodle_thickness_slot: thickness.b as usize,
            noodle_ridge_a_inner: self.op(ridge_a.a).a,
            noodle_ridge_a_slot: self.op(ridge_a.a).b as usize,
            noodle_ridge_b_inner: self.op(ridge_b.a).a,
            noodle_ridge_b_slot: self.op(ridge_b.a).b as usize,
        })
    }

    fn fixed_left(&self, id: NodeId, kind: OpKind, value: f64) -> Option<NodeId> {
        let op = self.op(id);
        (op.kind == kind && self.const_equal(op.a, value)).then_some(op.b)
    }

    fn stock_gradient(&self, id: NodeId, values: [f64; 4]) -> bool {
        let op = self.op(id);
        op.kind == OpKind::YClampedGradient
            && self.params[op.a as usize..op.a as usize + 4]
                .iter().zip(values).all(|(actual, expected)| actual.to_bits() == expected.to_bits())
    }

    fn bounded_stock_noise(&self, id: NodeId, xz: f64, y: f64) -> bool {
        let op = self.op(id);
        op.kind == OpKind::Noise && self.params_equal(op.b, xz, y)
            && self.noises[op.a as usize].conservative_stock_bound()
                .is_some_and(|bound| bound < 3.999)
    }

    fn bounded_entrances(&self, id: NodeId) -> Option<()> {
        let entrance = self.op(self.fixed_left(id, OpKind::Mul, 5.0)?);
        if entrance.kind != OpKind::Min { return None; }
        let first = self.op(entrance.a);
        if first.kind != OpKind::Add
            || !self.stock_gradient(first.b, [-10.0, 30.0, 0.3, 0.0])
            || !self.bounded_stock_noise(self.fixed_left(first.a, OpKind::Add, 0.37)?, 0.75, 0.5)
        { return None; }
        let second = self.op(entrance.b);
        if second.kind != OpKind::Add { return None; }
        let clamp = self.op(second.b);
        if clamp.kind != OpKind::Clamp || !self.params_equal(clamp.b, -1.0, 1.0) {
            return None;
        }
        let rough = self.op(second.a);
        if rough.kind != OpKind::Mul { return None; }
        let modulator = self.fixed_left(
            self.fixed_left(rough.a, OpKind::Add, -0.05)?, OpKind::Mul, -0.05,
        )?;
        let absolute = self.op(self.fixed_left(rough.b, OpKind::Add, -0.4)?);
        if absolute.kind != OpKind::Abs
            || !self.bounded_stock_noise(modulator, 1.0, 1.0)
            || !self.bounded_stock_noise(absolute.a, 1.0, 1.0)
        { return None; }
        // Clamp is bounded or NaN; the finite first arm wins a NaN in f64::min.
        Some(())
    }

    fn stock_terrain_range(&self, terrain: NodeId) -> Option<NodeId> {
        let bottom = self.op(self.fixed_left(
            self.fixed_left(terrain, OpKind::Mul, 0.64)?, OpKind::Add, 0.1171875,
        )?);
        if bottom.kind != OpKind::Mul
            || !self.stock_gradient(bottom.a, [-64.0, -40.0, 0.0, 1.0])
        { return None; }
        let top = self.op(self.fixed_left(
            self.fixed_left(bottom.b, OpKind::Add, -0.1171875)?, OpKind::Add, -0.078125,
        )?);
        if top.kind != OpKind::Mul
            || !self.stock_gradient(top.a, [240.0, 256.0, 1.0, 0.0])
        { return None; }
        let id = self.fixed_left(top.b, OpKind::Add, 0.078125)?;
        let range = self.op(id);
        if !matches!(range.kind, OpKind::RangeChoice | OpKind::DeepTerrainRangeChoice)
            || !self.params_equal(range.b, -1_000_000.0, 1.5625)
        {
            return None;
        }
        Some(id)
    }

    fn detect_pre_corner_slope(&self, terrain: NodeId) -> Option<NodeId> {
        if self.ops.iter().filter(|op| op.kind == OpKind::Blended).count() != 1 {
            return None;
        }
        let range = self.op(self.stock_terrain_range(terrain)?);
        let slope_id = self.child(range.a);
        let minimum = self.op(self.child(range.a + 1));
        if minimum.kind != OpKind::Min || minimum.a != slope_id { return None; }
        self.bounded_entrances(minimum.b)?;
        let slope = self.op(slope_id);
        if slope.kind != OpKind::Add { return None; }
        let blended = self.op(slope.b);
        if blended.kind != OpKind::Blended { return None; }
        let Density::Blended(noise) = &self.leaves[blended.a as usize] else { return None; };
        noise.conservative_overworld_bound()?;
        let quarter = self.op(self.fixed_left(slope.a, OpKind::Mul, 4.0)?);
        if quarter.kind != OpKind::QuarterNegative { return None; }
        let product = self.op(quarter.a);
        if product.kind != OpKind::Mul || self.op(product.b).kind != OpKind::FlatCache {
            return None;
        }
        let sum = self.op(product.a);
        if sum.kind != OpKind::Add || self.op(sum.b).kind != OpKind::FlatCache { return None; }
        let depth = self.op(sum.a);
        if depth.kind != OpKind::Add || self.op(depth.b).kind != OpKind::FlatCache
            || !self.stock_gradient(depth.a, [-64.0, 320.0, 1.5, -1.5])
        { return None; }
        Some(slope.a)
    }

    fn compile_deep_terrain_residual(&mut self) -> Option<DeepTerrainPlan> {
        let final_plan = self.overworld_final_density?;
        let non_blended = final_plan.pre_corner_slope?;
        let range = self.stock_terrain_range(final_plan.terrain_inner)?;
        let select = self.op(range);
        let sum = self.child(select.a);
        let slope = self.op(sum);
        if slope.kind != OpKind::Add || slope.a != non_blended { return None; }
        let blended = slope.b;
        let leaf = self.op(blended);
        if leaf.kind != OpKind::Blended { return None; }
        let Density::Blended(noise) = &self.leaves[leaf.a as usize] else { return None; };
        if noise.conservative_overworld_bound()? > 2.001 { return None; }
        let out = self.child(select.a + 2);
        let mut clamps = self.ops.iter().enumerate().filter_map(|(index, op)| {
            if op.kind != OpKind::Clamp || !self.params_equal(op.b, 0.0, 0.5) {
                return None;
            }
            let scaled = self.fixed_left(op.a, OpKind::Add, 1.5)?;
            (self.fixed_left(scaled, OpKind::Mul, -0.64)? == sum).then_some(index as NodeId)
        });
        let clamp = clamps.next()?;
        if clamps.next().is_some() { return None; }
        let mut spine = vec![None; self.ops.len()];
        if !self.deep_residual_spine(out, sum, blended, clamp, &mut spine)? {
            return None;
        }
        let mut counts = [0; 3];
        for (index, &marked) in spine.iter().enumerate() {
            if marked != Some(true) || index == clamp as usize { continue; }
            match self.ops[index].kind {
                OpKind::Add => counts[0] += 1,
                OpKind::Min => counts[1] += 1,
                OpKind::Max => counts[2] += 1,
                _ => return None,
            }
        }
        if counts != [2, 2, 1] { return None; }
        let mut remap: Vec<NodeId> = (0..self.ops.len() as NodeId).collect();
        remap[clamp as usize] = self.const_node(0.0);
        for (index, marked) in spine.into_iter().enumerate() {
            if marked != Some(true) || index == clamp as usize { continue; }
            let op = self.ops[index];
            remap[index] = self.push(op.kind, remap[op.a as usize], remap[op.b as usize], op.c);
        }
        Some(DeepTerrainPlan { range, non_blended, blended, residual: remap[out as usize] })
    }

    fn deep_residual_spine(
        &self, id: NodeId, sum: NodeId, blended: NodeId, clamp: NodeId,
        seen: &mut [Option<bool>],
    ) -> Option<bool> {
        if id == sum || id == blended { return None; }
        if id == clamp {
            seen[id as usize] = Some(true);
            return Some(true);
        }
        if let Some(marked) = seen[id as usize] { return Some(marked); }
        let op = self.op(id);
        let mut marked = false;
        let mut visit = |child| -> Option<()> {
            marked |= self.deep_residual_spine(child, sum, blended, clamp, seen)?;
            Some(())
        };
        match op.kind {
            OpKind::Add | OpKind::Mul | OpKind::Min | OpKind::Max => {
                visit(op.a)?;
                visit(op.b)?;
            }
            OpKind::Abs | OpKind::Square | OpKind::Cube | OpKind::HalfNegative
            | OpKind::QuarterNegative | OpKind::Squeeze | OpKind::Invert
            | OpKind::Clamp | OpKind::Interpolated | OpKind::FlatCache => visit(op.a)?,
            OpKind::ShiftedNoise | OpKind::RangeChoice => {
                for offset in 0..3 { visit(self.child(op.a + offset))?; }
            }
            OpKind::IntervalSelect => {
                for offset in 0..=self.child(op.a) {
                    visit(self.child(op.a + 1 + offset))?;
                }
            }
            OpKind::Const | OpKind::BlendAlpha | OpKind::BlendOffset | OpKind::Beardifier
            | OpKind::YClampedGradient | OpKind::Noise
            | OpKind::ShiftA | OpKind::ShiftB | OpKind::Shift => {}
            OpKind::Spline | OpKind::Blended | OpKind::FindTopSurface | OpKind::EndIslands
            | OpKind::DeepTerrainRangeChoice => {
                return None;
            }
        }
        if marked && !matches!(op.kind, OpKind::Add | OpKind::Min | OpKind::Max) {
            return None;
        }
        seen[id as usize] = Some(marked);
        Some(marked)
    }

    fn const_equal(&self, id: NodeId, expected: f64) -> bool {
        let op = self.op(id);
        op.kind == OpKind::Const
            && self.params.get(op.a as usize).copied().map(f64::to_bits)
                == Some(expected.to_bits())
    }

    fn params_equal(&self, offset: u32, first: f64, second: f64) -> bool {
        self.params.get(offset as usize).copied().map(f64::to_bits)
            == Some(first.to_bits())
            && self
                .params
                .get(offset as usize + 1)
                .copied()
                .map(f64::to_bits)
                == Some(second.to_bits())
    }

    fn mark_clamp_max_rhs_first(&mut self, pure: &[bool]) {
        for index in 0..self.ops.len() {
            let clamp = self.ops[index];
            if clamp.kind != OpKind::Clamp {
                continue;
            }
            let maximum = self.op(clamp.a);
            let low = self.params[clamp.b as usize];
            let high = self.params[clamp.b as usize + 1];
            if maximum.kind == OpKind::Max
                && pure[maximum.a as usize]
                && low.is_finite()
                && high.is_finite()
                && low <= high
            {
                self.ops[index].c = CLAMP_MAX_RHS_FIRST;
            }
        }
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

    fn const_node(&mut self, value: f64) -> NodeId {
        let p = self.push_params(&[value]);
        self.push(OpKind::Const, p, 0, 0)
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

    fn push_spline(&mut self, d: &Density) -> u32 {
        let mut sig = Vec::new();
        d.write_signature(&mut sig);
        let key: Box<[u64]> = sig.into();
        if let Some(&at) = self.interner.splines.get(&key) {
            self.interner.shared_splines += 1;
            return at;
        }
        let at = self.splines.len() as u32;
        self.splines.push(PointProgram::compile(d));
        self.interner.splines.insert(key, at);
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
    /// | unary arithmetic | child id | — | — |
    /// | `Marker`/`Cache2D` | transparent; omitted from the field graph |
    /// | `Clamp` | child id | params\[2\] | rhs-first marker |
    /// | `Interpolated`/`FlatCache` | child id | slot | — |
    /// | `Noise` | noise idx | params\[2\] | — |
    /// | `ShiftedNoise` | children\[3\] | noise idx | params\[2\] |
    /// | `ShiftA`/`ShiftB`/`Shift` | noise idx | — | — |
    /// | `RangeChoice` | children\[3\] | params\[2\] | — |
    /// | `IntervalSelect` | children\[n\] | n | params\[n-1\] |
    /// | `Spline` | compiled spline idx | — | — |
    /// | `Blended`/`FindTopSurface`/`EndIslands` | leaf idx | — | — |
    fn compile_node(&mut self, d: &Density) -> NodeId {
        // Resolve constant-only subtrees before emitting their children. This
        // is deliberately side-effect-aware: interpolated and flat-cache nodes
        // are not constants here because evaluating either writes a sampler
        // cache even when its value happens to be constant. A selector whose
        // input is constant is safe to route directly; the discarded branch
        // was unreachable in the reference walk anyway.
        if !matches!(
            d,
            Density::Const(_)
                | Density::BlendAlpha
                | Density::BlendOffset
                | Density::Beardifier
        ) && let Some(value) = constant_value(d)
        {
            self.interner.constant_folds += 1;
            return self.const_node(value);
        }
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
                let id = self.push(OpKind::FlatCache, child, u32::try_from(*slot).unwrap(), 0);
                if let Some(manifest) = self.product_manifest.as_ref()
                    && let Some(kind) = manifest.kind_for(inner)
                {
                    self.product_nodes.push((id, kind));
                }
                id
            }
            // Both wrappers are transparent in the block-field evaluator.
            // `cache_2d` remains meaningful to the point interpreter, but this
            // graph never enters a point leaf, so retaining either wrapper only
            // adds a dispatch per field visit. A product label is retained on
            // the surviving child so the field evaluator can answer it from
            // the request lattice without reintroducing the wrapper's cache.
            Density::Cache2D { inner, .. } => {
                let child = self.compile_node(inner);
                if let Some(manifest) = self.product_manifest.as_ref()
                    && let Some(kind) = manifest.kind_for(inner)
                {
                    self.product_nodes.push((child, kind));
                }
                child
            }
            Density::Marker(inner) => self.compile_node(inner),
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
                if let Some(value) = constant_value(input) {
                    let selected = if value >= *min_inclusive && value < *max_exclusive {
                        when_in_range
                    } else {
                        when_out_of_range
                    };
                    return self.compile_node(selected);
                }
                let cin = self.compile_node(when_in_range);
                let cout = self.compile_node(when_out_of_range);
                if cin == cout && !contains_field_cache_writer(input) {
                    // The input is still evaluated by the unoptimised walk,
                    // but it cannot write a field cache. Both branches denote
                    // the same compiled function, so the selector adds only a
                    // dispatch and a dead input walk.
                    return cin;
                }
                let ci = self.compile_node(input);
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
                let n = u32::try_from(functions.len()).unwrap();
                if let Some(value) = constant_value(input)
                    && n > 0
                {
                    let index = thresholds
                        .iter()
                        .position(|threshold| value < *threshold)
                        .unwrap_or(functions.len() - 1);
                    if let Some(selected) = functions.get(index) {
                        return self.compile_node(selected);
                    }
                }
                let mut ids = Vec::with_capacity(functions.len() + 2);
                ids.push(n);
                for f in functions {
                    ids.push(self.compile_node(f));
                }
                if n > 0
                    && ids[1..].windows(2).all(|pair| pair[0] == pair[1])
                    && !contains_field_cache_writer(input)
                {
                    return ids[1];
                }
                let ci = self.compile_node(input);
                ids.insert(1, ci);
                let kids = self.push_children(&ids);
                let p = self.push_params(thresholds);
                self.push(
                    OpKind::IntervalSelect,
                    kids,
                    p,
                    u32::try_from(thresholds.len()).unwrap(),
                )
            }
            // Spline remains a point-semantics boundary, but its recursive
            // control data is compiled into indexed point arrays.
            Density::Spline(_) => {
                let spline = self.push_spline(d);
                self.push(OpKind::Spline, spline, 0, 0)
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
            | OpKind::Clamp => self.walk_interpolating(op.a, interpolate, out),
            OpKind::ShiftedNoise => {
                for i in 0..3 {
                    self.walk_interpolating(self.child(op.a + i), interpolate, out);
                }
            }
            OpKind::RangeChoice | OpKind::DeepTerrainRangeChoice => {
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

    fn stock_pre_corner_program() -> (Program, usize) {
        use crate::density::{Builder, NoiseParams, Resolver};
        use serde_json::Value;
        struct Files(std::path::PathBuf);
        impl Files {
            fn read(&self, kind: &str, id: &str) -> Value {
                let id = id.strip_prefix("minecraft:").unwrap_or(id);
                serde_json::from_str(&std::fs::read_to_string(
                    self.0.join(kind).join(format!("{id}.json")),
                ).unwrap()).unwrap()
            }
        }
        impl Resolver for Files {
            fn density_function(&self, id: &str) -> Value { self.read("density_function", id) }
            fn noise(&self, id: &str) -> NoiseParams {
                let value = self.read("noise", id);
                NoiseParams {
                    first_octave: value["firstOctave"].as_i64().unwrap() as i32,
                    amplitudes: value["amplitudes"].as_array().unwrap().iter()
                        .map(|v| v.as_f64().unwrap()).collect(),
                }
            }
        }
        let files = Files(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../lodestone-worldgen/tests/support/worldgen_data"));
        let settings = files.read("noise_settings", "overworld");
        let builder = Builder::new(42, &files);
        let density = builder.build(&settings["noise_router"]["final_density"]).unwrap();
        (Program::compile(&density), builder.slot_count())
    }

    #[test]
    fn deep_saturation_threshold_has_an_independent_margin() {
        let (program, _) = stock_pre_corner_program();
        let plan = program.graph().deep_terrain_plan().unwrap();
        let worst_sum = 4.345_f64 - 2.001_f64;
        assert!(worst_sum >= 1.5625);
        assert_eq!((1.5_f64 + (-0.64_f64 * worst_sum)).clamp(0.0, 0.5).to_bits(),
            0.0_f64.to_bits());
        assert!((1.5_f64 + (-0.64_f64 * (4.344_f64 - 2.001_f64))).clamp(0.0, 0.5) > 0.0);
        assert!(plan.admits(4.345) && plan.admits(65536.0));
        for value in [4.344, 65536.0001, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(!plan.admits(value));
        }
    }

    #[test]
    fn deep_saturation_matcher_clones_only_the_five_binary_ancestors() {
        fn compare(graph: &Graph, original: NodeId, residual: NodeId) -> usize {
            if original == residual { return 0; }
            let a = graph.op(original);
            let b = graph.op(residual);
            if a.kind == OpKind::Clamp {
                assert!(graph.params_equal(a.b, 0.0, 0.5));
                assert!(graph.const_equal(residual, 0.0));
                return 0;
            }
            assert_eq!(a.kind, b.kind);
            assert!(matches!(a.kind, OpKind::Add | OpKind::Min | OpKind::Max));
            1 + compare(graph, a.a, b.a) + compare(graph, a.b, b.b)
        }
        let (program, _) = stock_pre_corner_program();
        let graph = program.graph();
        let plan = graph.deep_terrain_plan().unwrap();
        let out = graph.child(graph.op(plan.range).a + 2);
        assert_eq!(graph.op(plan.range).kind, OpKind::DeepTerrainRangeChoice);
        assert_eq!(graph.op(plan.range).kind.density_kind(), OpKind::RangeChoice as usize);
        assert_eq!(graph.ops.iter().filter(|op| op.kind == OpKind::DeepTerrainRangeChoice).count(), 1);
        assert!(program.count_kind("range_choice") > 0);
        assert_eq!(compare(graph, out, plan.residual), 5);
        assert_eq!(graph.ops.iter().filter(|op| op.kind == OpKind::Blended).count(), 1);
        assert!(!graph.tile_eligible(plan.residual));
    }

    #[test]
    fn deep_saturation_matcher_proves_all_selector_dependencies() {
        for offset in 0..3 {
            let (mut program, _) = stock_pre_corner_program();
            let graph = Arc::get_mut(&mut program.graph).unwrap();
            let plan = graph.deep_terrain_plan().unwrap();
            let select = graph.op(plan.range);
            let sum = graph.child(select.a);
            let out = graph.child(select.a + 2);
            let clamp = graph.ops.iter().enumerate().find_map(|(id, op)| {
                (op.kind == OpKind::Clamp && graph.params_equal(op.b, 0.0, 0.5)
                    && graph.fixed_left(op.a, OpKind::Add, 1.5)
                        .and_then(|id| graph.fixed_left(id, OpKind::Mul, -0.64)) == Some(sum))
                    .then_some(id as NodeId)
            }).unwrap();
            let mut seen = vec![None; graph.ops.len()];
            assert_eq!(graph.deep_residual_spine(out, sum, plan.blended, clamp, &mut seen),
                Some(true));
            let nested = seen.iter().enumerate().find_map(|(id, seen)| {
                (seen.is_some() && id as NodeId != plan.range
                    && graph.ops[id].kind == OpKind::RangeChoice).then_some(id)
            }).expect("stock out branch must contain a nested selector");
            let start = graph.ops[nested].a as usize;
            graph.children[start + offset] = sum;
            assert!(graph.compile_deep_terrain_residual().is_none(),
                "selector dependency at child {offset} must decline");
        }
        let (mut program, _) = stock_pre_corner_program();
        let graph = Arc::get_mut(&mut program.graph).unwrap();
        let plan = graph.deep_terrain_plan().unwrap();
        let out = graph.child(graph.op(plan.range).a + 2);
        graph.ops[out as usize].a = plan.blended;
        assert!(graph.compile_deep_terrain_residual().is_none(), "direct noise escape");
        graph.ops[out as usize].kind = OpKind::Spline;
        assert!(graph.compile_deep_terrain_residual().is_none(), "opaque dependency");
        let (mut program, _) = stock_pre_corner_program();
        let graph = Arc::get_mut(&mut program.graph).unwrap();
        let plan = graph.deep_terrain_plan().unwrap();
        let sum = graph.child(graph.op(plan.range).a);
        let clamp = graph.ops.iter().enumerate().find_map(|(id, op)| {
            (op.kind == OpKind::Clamp && graph.params_equal(op.b, 0.0, 0.5)
                && graph.fixed_left(op.a, OpKind::Add, 1.5)
                    .and_then(|id| graph.fixed_left(id, OpKind::Mul, -0.64)) == Some(sum))
                .then_some(id)
        }).unwrap();
        let bounds = graph.params.len() as u32;
        graph.params.extend([-0.0, 0.5]);
        graph.ops[clamp].b = bounds;
        assert!(graph.compile_deep_terrain_residual().is_none(), "wrong zero bit pattern");
    }

    #[test]
    fn deep_saturation_stock_exact_demands_and_publications() {
        use crate::engine::{Bounds, Field, Geom, Scratch};
        use crate::noise::improved::capture_samples;
        fn run(program: &Program, scratch: &mut Scratch, enabled: bool,
            x: i32, y: i32, z: i32, proofs: bool)
            -> (u8, [u64; 128], Vec<(usize, (i32, i32, i32), u64)>, Vec<(i32, i32, i32)>)
        {
            let plan = program.overworld_final_density_plan().unwrap();
            let mut field = Field::new(program.graph(),
                Geom { cell_width: 4, cell_height: 8 }, scratch);
            field.deep_saturation_enabled = enabled;
            field.publications = Some(Vec::new());
            field.blended_queries = Some(Vec::new());
            let mut values = [f64::from_bits(0x7ff8_0000_0000_0011); 128];
            let class = if proofs && y >= 40 && (field.pre_corner_terrain_is_negative(plan, x, y, z)
                || field.eval_overworld_final_density_cell_terrain_is_nonpositive(plan, x, y, z))
            {
                0
            } else if proofs && field.eval_overworld_final_density_cell_is_positive(plan, x, y, z) {
                1
            } else {
                field.eval_overworld_final_density_cell(plan, x, y, z, &mut values);
                2
            };
            (class, values.map(f64::to_bits), field.publications.take().unwrap(),
                field.blended_queries.take().unwrap())
        }
        let (program, slots) = stock_pre_corner_program();
        let deep = program.graph().deep_terrain_plan().unwrap();
        let leaf = program.graph().op(deep.blended);
        let Density::Blended(noise) = &program.graph().leaves[leaf.a as usize] else { unreachable!() };
        let bounds = Bounds { x: (-4, 7), y: (-64, 255), z: (-4, 7) };
        for proofs in [true, false] {
            let mut baseline = Scratch::acquire(slots, 4, 8, Some(bounds));
            let mut candidate = Scratch::acquire(slots, 4, 8, Some(bounds));
            let mut base_points = std::collections::BTreeSet::new();
            let mut candidate_points = std::collections::BTreeSet::new();
            let mut baseline_calls = 0;
            let mut candidate_calls = 0;
            for (x, z) in [(-4, -4), (0, -4), (-4, 0), (0, 0)] {
                for y in (-64..=248).step_by(8) {
                    let (expected, base_trace) = capture_samples(||
                        run(&program, &mut baseline, false, x, y, z, proofs));
                    let (actual, candidate_trace) = capture_samples(||
                        run(&program, &mut candidate, true, x, y, z, proofs));
                    assert_eq!((&actual.0, &actual.1, &actual.2),
                        (&expected.0, &expected.1, &expected.2),
                        "class/bits/publications at ({x},{y},{z}), proofs={proofs}");
                    let (_, blended_trace) = capture_samples(|| {
                        for &(px, py, pz) in &expected.3 {
                            let _ = noise.compute(px, py, pz);
                        }
                    });
                    let blended_ids: std::collections::BTreeSet<_> =
                        blended_trace.iter().map(|sample| sample.0).collect();
                    let remaining = |trace: Vec<(usize, i32, u64, u64)>| -> Vec<_> {
                        trace.into_iter().filter(|sample| !blended_ids.contains(&sample.0)).collect()
                    };
                    assert_eq!(remaining(candidate_trace), remaining(base_trace),
                        "remaining noise call order/arithmetic at ({x},{y},{z})");
                    let expected_points: std::collections::BTreeSet<_> = expected.3.iter().copied().collect();
                    assert!(actual.3.iter().all(|point| expected_points.contains(point)),
                        "residual must not add demanded coordinates");
                    baseline_calls += expected.3.len();
                    candidate_calls += actual.3.len();
                    base_points.extend(expected.3);
                    candidate_points.extend(actual.3);
                    let warm_base = run(&program, &mut baseline, false, x, y, z, proofs);
                    let warm_candidate = run(&program, &mut candidate, true, x, y, z, proofs);
                    assert_eq!(warm_candidate, warm_base, "warm cell ({x},{y},{z})");
                    assert!(warm_candidate.3.is_empty());
                }
            }
            assert!(candidate_points.is_subset(&base_points));
            assert!(base_points.len() > candidate_points.len() && baseline_calls > candidate_calls,
                "complete traversal must remove real misses, proofs={proofs}");
            baseline.release();
            candidate.release();
        }
    }

    #[cfg(feature = "gen-counters")]
    #[test]
    fn deep_saturation_shadow_counts_the_complete_traversal() {
        use crate::density::NoiseChunkRegionSampler;
        use crate::engine::Bounds;
        let (program, slots) = stock_pre_corner_program();
        let bounds = Bounds { x: (-4, 7), y: (-64, 255), z: (-4, 7) };
        let mut region = NoiseChunkRegionSampler::from_program(program, slots, 4, 8, bounds);
        region.enable_deep_saturation_shadow(slots);
        for (x, z) in [(-4, -4), (0, -4), (-4, 0), (0, 0)] {
            for y in (-64..=248).step_by(8) {
                assert!(region.pre_corner_shadow_cell(x, y, z, true, y >= 40).is_none());
            }
        }
        let (avoided, added, baseline, candidate) = region.deep_saturation_shadow_counts().unwrap();
        assert!(avoided > 0 && baseline > candidate, "full traversal must omit actual noise misses");
        assert_eq!(added, 0, "no additional demanded coordinate");
    }

    #[test]
    fn deep_saturation_does_not_publish_a_synthetic_selector() {
        use crate::engine::{Field, Geom, Scratch};
        let (mut program, slots) = stock_pre_corner_program();
        let graph = Arc::get_mut(&mut program.graph).unwrap();
        let deep = graph.deep_terrain_plan().unwrap();
        let param = graph.params.len() as u32;
        graph.params.push(8.0);
        graph.ops[deep.non_blended as usize] = Op { kind: OpKind::Const, a: param, b: 0, c: 0 };
        let sum = graph.child(graph.op(deep.range).a);
        let terrain = graph.overworld_final_density.unwrap().terrain_inner;
        let leaf = graph.op(deep.blended);
        let Density::Blended(noise) = &graph.leaves[leaf.a as usize] else { unreachable!() };
        let expected = 8.0 + noise.compute(0, 0, 0);
        let mut scratch = Scratch::acquire(slots, 4, 8, None);
        {
            let mut field = Field::new(graph, Geom { cell_width: 4, cell_height: 8 }, &mut scratch);
            field.blended_queries = Some(Vec::new());
            field.eval::<false>(terrain, 0, 0, 0);
            assert!(field.blended_queries.as_ref().unwrap().is_empty());
            assert_eq!(field.eval::<false>(sum, 0, 0, 0).to_bits(), expected.to_bits());
            assert_eq!(field.blended_queries.take().unwrap(), [(0, 0, 0)]);
        }
        scratch.release();
    }

    #[test]
    #[should_panic(expected = "forced deep residual escaped detector")]
    fn deep_saturation_forced_invalid_residual_is_detected() {
        use crate::engine::{Field, Geom, Scratch};
        let (mut program, slots) = stock_pre_corner_program();
        let graph = Arc::get_mut(&mut program.graph).unwrap();
        let deep = graph.deep_terrain_plan().unwrap();
        let param = graph.params.len() as u32;
        graph.params.push(-4.0);
        graph.ops[deep.non_blended as usize] = Op { kind: OpKind::Const, a: param, b: 0, c: 0 };
        let plan = graph.overworld_final_density.unwrap();
        let mut baseline = Scratch::acquire(slots, 4, 8, None);
        let mut candidate = Scratch::acquire(slots, 4, 8, None);
        let predicted = (-0.5_f64 + 1.0_f64 / 24.0_f64).to_bits();
        let mut witness = None;
        for x in (0..64).step_by(4) {
            let mut reference = [0.0; 128];
            let mut actual = [0.0; 128];
            {
                let mut field = Field::new(graph, Geom { cell_width: 4, cell_height: 8 }, &mut baseline);
                field.eval_overworld_final_density_cell(plan, x, 0, 0, &mut reference);
            }
            {
                let mut field = Field::new(graph, Geom { cell_width: 4, cell_height: 8 }, &mut candidate);
                field.force_deep_saturation = true;
                field.eval_overworld_final_density_cell(plan, x, 0, 0, &mut actual);
            }
            assert!(reference.iter().all(|value| value.to_bits() == predicted),
                "independent clamped terrain prediction");
            if let Some(lane) = (0..128).find(|&lane| reference[lane].to_bits() != actual[lane].to_bits()) {
                witness = Some((reference[lane].to_bits(), actual[lane].to_bits(), x, lane));
                break;
            }
        }
        baseline.release();
        candidate.release();
        let (expected, actual, x, lane) = witness.expect("forced residual needs a real density witness");
        assert_eq!(actual, expected, "forced deep residual escaped detector at ({x},0,0) lane {lane}");
    }

    #[cfg(feature = "gen-counters")]
    #[test]
    fn pre_corner_matcher_requires_the_identical_selector_in_the_minimum() {
        let (mut program, _) = stock_pre_corner_program();
        let graph = Arc::get_mut(&mut program.graph).unwrap();
        let plan = graph.overworld_final_density.unwrap();
        assert!(plan.pre_corner_slope.is_some());
        let range = graph.ops.iter().copied().find(|op| {
            matches!(op.kind, OpKind::RangeChoice | OpKind::DeepTerrainRangeChoice)
                && graph.params_equal(op.b, -1_000_000.0, 1.5625)
        }).unwrap();
        let minimum = graph.child(range.a + 1);
        graph.ops[minimum as usize].a = plan.pre_corner_slope.unwrap();
        assert!(graph.detect_pre_corner_slope(plan.terrain_inner).is_none());
    }

    #[test]
    fn pre_corner_skip_preserves_later_exact_density_bits() {
        use crate::density::NoiseChunkSampler;
        use crate::engine::{Bounds, Field, Geom, Scratch};
        let (program, slots) = stock_pre_corner_program();
        let plan = program.overworld_final_density_plan().unwrap();
        let bounds = Bounds { x: (-4, 7), y: (224, 255), z: (-4, 7) };
        let mut scratch = Scratch::acquire(slots, 4, 8, Some(bounds));
        {
            let mut field = Field::new(
                program.graph(), Geom { cell_width: 4, cell_height: 8 }, &mut scratch,
            );
            assert!(field.pre_corner_terrain_is_negative(plan, -4, 232, -4));
        }
        assert!(scratch.cell_get(plan.terrain_slot, -1, 29, -1).is_none());
        for x in [-4, 0] {
            for y in [232, 240] {
                for z in [-4, 0] {
                    assert!(scratch.slot_get(plan.terrain_slot, (x, y, z)).is_none());
                }
            }
        }
        scratch.release();

        let skipped = NoiseChunkSampler::from_program(program.clone(), slots, 4, 8, Some(bounds));
        let exact = NoiseChunkSampler::from_program(program, slots, 4, 8, Some(bounds));
        assert!(skipped.final_density_cell_terrain_is_nonpositive(-4, 232, -4));
        for (x, y, z) in [(-4, 232, -4), (0, 232, -4), (-4, 224, -4), (-4, 232, 0)] {
            let mut actual = [0.0; 128];
            skipped.final_density_cell(x, y, z, &mut actual);
            for (lane, value) in actual.into_iter().enumerate() {
                let px = x + ((lane % 32) / 8) as i32;
                let py = y + (lane % 8) as i32;
                let pz = z + (lane / 32) as i32;
                assert_eq!(value.to_bits(), exact.final_density(px, py, pz).to_bits(),
                    "exact query after certificate at ({px},{py},{pz})");
            }
        }
    }

    #[test]
    fn prepared_vertical_stock_preserves_demands_and_publications() {
        use crate::engine::{Bounds, Field, Geom, PreparedBlendedCellColumn, Scratch};
        assert!(std::mem::size_of::<PreparedBlendedCellColumn>() <= 8192);
        fn run(program: &Program, plan: OverworldFinalDensityPlan, scratch: &mut Scratch,
            column: Option<&mut PreparedBlendedCellColumn>, x: i32, y: i32, z: i32)
            -> (u8, [u64; 128], Vec<(usize, (i32, i32, i32), u64)>, Vec<(i32, i32, i32)>)
        {
            let mut field = Field::new(program.graph(),
                Geom { cell_width: 4, cell_height: 8 }, scratch).with_blended_column(column);
            field.publications = Some(Vec::new());
            field.blended_queries = Some(Vec::new());
            let mut values = [f64::from_bits(0x7ff8_0000_0000_0011); 128];
            let class = if y >= 40 && (field.pre_corner_terrain_is_negative(plan, x, y, z)
                || field.eval_overworld_final_density_cell_terrain_is_nonpositive(plan, x, y, z))
            {
                1
            } else if field.eval_overworld_final_density_cell_is_positive(plan, x, y, z) {
                2
            } else {
                field.eval_overworld_final_density_cell(plan, x, y, z, &mut values);
                3
            };
            (class, values.map(f64::to_bits), field.publications.take().unwrap(),
                field.blended_queries.take().unwrap())
        }

        let (program, slots) = stock_pre_corner_program();
        let plan = program.overworld_final_density_plan().unwrap();
        let node = plan.prepared_blended.expect("stock prepared blended node");
        let bounds = Bounds { x: (-4, 7), y: (-64, 255), z: (-4, 7) };
        let mut ordinary = Scratch::acquire(slots, 4, 8, Some(bounds));
        let mut prepared = Scratch::acquire(slots, 4, 8, Some(bounds));
        let mut skipped_column = PreparedBlendedCellColumn::new(node, -4, -4);
        let skipped_control = run(&program, plan, &mut ordinary, None, -4, 232, -4);
        let skipped = run(&program, plan, &mut prepared, Some(&mut skipped_column), -4, 232, -4);
        assert_eq!(skipped, skipped_control);
        assert_eq!(skipped.0, 1);
        assert!(skipped.3.is_empty());
        assert_eq!(skipped_column.prepared_count(), 0, "certificate must not construct operands");
        let mut certificates = 0;
        let mut mixed = 0;
        let mut demands = 0;
        let mut operand_count = 0;
        for (x, z) in [(-4, -4), (0, -4), (-4, 0), (0, 0)] {
            let mut column = PreparedBlendedCellColumn::new(node, x, z);
            assert_eq!(column.prepared_count(), 0);
            let mut demanded_xz = std::collections::BTreeSet::new();
            for y in (-64..=248).step_by(8) {
                let expected = run(&program, plan, &mut ordinary, None, x, y, z);
                let actual = run(&program, plan, &mut prepared, Some(&mut column), x, y, z);
                assert_eq!(actual, expected, "cell ({x},{y},{z}): class/bits/publications/demands");
                certificates += usize::from(actual.0 == 1);
                mixed += usize::from(actual.0 == 3);
                demands += actual.3.len();
                demanded_xz.extend(actual.3.iter().map(|&(px, _, pz)| (px, pz)));
                assert_eq!(column.prepared_count(), demanded_xz.len(), "only demanded X/Z may prepare");
                assert!(column.prepared_count() <= 4);
                let warm = run(&program, plan, &mut prepared, Some(&mut column), x, y, z);
                let warm_control = run(&program, plan, &mut ordinary, None, x, y, z);
                assert_eq!(warm, warm_control, "warm cell ({x},{y},{z})");
                assert!(warm.3.is_empty(), "warm cell must not demand blended noise");
            }
            operand_count += column.prepared_count();
        }
        assert!(certificates > 0 && mixed > 0 && demands > operand_count && operand_count > 0,
            "fixture must exercise certificates, mixed cells, and vertical operand reuse");
        ordinary.release();
        prepared.release();
    }

    #[test]
    fn prepared_vertical_cell_column_facade_matches_generic_and_stock_routes() {
        use crate::density::{Density, NoiseChunkRegionSampler};
        use crate::engine::Bounds;
        let (stock, slots) = stock_pre_corner_program();
        for (program, slots) in [(stock, slots), (Program::compile(&Density::Const(-0.125)), 0)] {
            let bounds = Bounds { x: (-4, -1), y: (0, 255), z: (-4, -1) };
            let prepared = NoiseChunkRegionSampler::from_program(program.clone(), slots, 4, 8, bounds);
            let ordinary = NoiseChunkRegionSampler::from_program(program, slots, 4, 8, bounds);
            prepared.with_cell_column(-4, -4, |column| {
                for y in [0, 8, 64, 232, 240, 248] {
                    assert_eq!(column.final_density_cell_terrain_is_nonpositive(-4, y, -4),
                        ordinary.final_density_cell_terrain_is_nonpositive(-4, y, -4));
                    let mut actual = [0.0; 128];
                    let mut expected = [0.0; 128];
                    assert_eq!(column.final_density_cell_or_positive(-4, y, -4, &mut actual),
                        ordinary.final_density_cell_or_positive(-4, y, -4, &mut expected));
                    assert_eq!(actual.map(f64::to_bits), expected.map(f64::to_bits));
                    column.final_density_cell(-4, y, -4, &mut actual);
                    ordinary.final_density_cell(-4, y, -4, &mut expected);
                    assert_eq!(actual.map(f64::to_bits), expected.map(f64::to_bits));
                }
            });
        }
    }

    #[test]
    fn prepared_vertical_matcher_declines_nonstock_noise() {
        let (mut program, _) = stock_pre_corner_program();
        let root = program.root();
        let graph = Arc::get_mut(&mut program.graph).unwrap();
        let node = graph.overworld_final_density.unwrap().prepared_blended.unwrap();
        let leaf = graph.ops[node as usize].a as usize;
        let mut rng = crate::rng::LegacyRandomSource::new(42);
        graph.leaves[leaf] = Density::Blended(crate::noise::BlendedNoise::new(
            &mut rng, 0.25, 0.125, 80.0, 160.0, 7.0,
        ));
        let plan = graph.detect_overworld_final_density(root).expect("cell shape still supported");
        assert!(plan.prepared_blended.is_none());
    }

    fn b(d: Density) -> Box<Density> {
        Box::new(d)
    }

    fn varying() -> Density {
        Density::YClampedGradient {
            from_y: -1.0,
            to_y: 1.0,
            from_value: -1.0,
            to_value: 1.0,
        }
    }

    fn varying_alt() -> Density {
        Density::YClampedGradient {
            from_y: -2.0,
            to_y: 2.0,
            from_value: -1.0,
            to_value: 1.0,
        }
    }

    /// Every emitted [`OpKind`] discriminant must equal the
    /// [`Density::kind_index`] of the variant it compiles from, so the
    /// `density_evals` per-kind counter reads the same bucket before and after
    /// flattening. Transparent wrappers are deliberately not emitted.
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
            (OpKind::Add, Density::Add(b(varying()), b(varying()))),
            (OpKind::Mul, Density::Mul(b(varying()), b(varying()))),
            (OpKind::Min, Density::Min(b(varying()), b(varying()))),
            (OpKind::Max, Density::Max(b(varying()), b(varying()))),
            (OpKind::Abs, Density::Abs(b(varying()))),
            (OpKind::Square, Density::Square(b(varying()))),
            (OpKind::Cube, Density::Cube(b(varying()))),
            (OpKind::HalfNegative, Density::HalfNegative(b(varying()))),
            (
                OpKind::QuarterNegative,
                Density::QuarterNegative(b(varying())),
            ),
            (OpKind::Squeeze, Density::Squeeze(b(varying()))),
            (OpKind::Invert, Density::Invert(b(varying()))),
            (
                OpKind::Clamp,
                Density::Clamp {
                    input: b(varying()),
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
                OpKind::RangeChoice,
                Density::RangeChoice {
                    input: b(varying()),
                    min_inclusive: 0.0,
                    max_exclusive: 1.0,
                    when_in_range: b(varying()),
                    when_out_of_range: b(Density::Const(0.0)),
                },
            ),
            (
                OpKind::IntervalSelect,
                Density::IntervalSelect {
                    input: b(varying()),
                    thresholds: vec![0.0],
                    functions: vec![varying(), Density::Const(1.0)],
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

        // The seven noise/spline payload kinds need a resolver; the two
        // transparent wrappers are intentionally absent from the field graph.
        assert_eq!(
            cases.len(),
            23,
            "the case list changed size; 23 of the 32 kinds are emitted by the \
             field graph without a resolver (the 7 needing a \
             NormalNoise/BlendedNoise/Spline payload are covered by \
             `compiles_the_real_router` in tests/engine_semantics.rs)"
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
            b(Density::Mul(b(varying()), b(Density::Const(3.0)))),
            b(Density::Abs(b(varying_alt()))),
        );
        let p = Program::compile(&d);
        assert_eq!(p.node_count(), 6, "2 gradients + 1 const + mul + abs + add");
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
                    .eval::<true>(p.root(), x, y, z);
            let tree = d.compute(crate::density::Context::new(x, y, z));
            assert_eq!(flat.to_bits(), tree.to_bits(), "at ({x}, {y}, {z})");
        }
        scratch.release();
    }

    /// Constant folding must preserve the evaluator's signed-zero result. The
    /// source operands compare equal under `==`, but the addition's positive
    /// zero result is an IEEE value that the folded node must retain.
    #[test]
    fn constant_folding_preserves_signed_zero_result() {
        let d = Density::Add(b(Density::Const(0.0)), b(Density::Const(-0.0)));
        let p = Program::compile(&d);
        assert_eq!(p.node_count(), 1, "the constant addition folds before emission");
        assert_eq!(p.graph().op(p.root()).kind, OpKind::Const);
        assert_eq!(p.graph().params()[p.graph().op(p.root()).a as usize].to_bits(), 0.0f64.to_bits());
        assert_eq!(p.shared_nodes(), 0);

        // Control: an equal-valued addition folds to the same single constant,
        // so this checks the result is not an accidental signed-zero special case.
        let same = Density::Add(b(Density::Const(0.0)), b(Density::Const(0.0)));
        let q = Program::compile(&same);
        assert_eq!(q.node_count(), 1, "the equal constant addition also folds");
        assert_eq!(q.shared_nodes(), 0);

        let skipped = Density::Mul(
            b(Density::Const(-0.0)),
            b(Density::Interpolated {
                inner: b(Density::Const(99.0)),
                slot: 0,
            }),
        );
        let r = Program::compile(&skipped);
        assert_eq!(r.node_count(), 1, "a zero first operand skips its right subtree");
        assert_eq!(r.graph().params()[r.graph().op(r.root()).a as usize].to_bits(), 0.0f64.to_bits());
    }

    #[test]
    fn constant_selector_keeps_selected_cache_writer_and_drops_unreachable_branch() {
        let d = Density::RangeChoice {
            input: b(Density::Const(0.5)),
            min_inclusive: 0.0,
            max_exclusive: 1.0,
            when_in_range: b(Density::Interpolated {
                inner: b(Density::Const(2.0)),
                slot: 0,
            }),
            when_out_of_range: b(Density::Add(
                b(Density::Const(20.0)),
                b(Density::Const(22.0)),
            )),
        };
        let p = Program::compile(&d);
        assert_eq!(p.graph().op(p.root()).kind, OpKind::Interpolated);
        assert_eq!(p.node_count(), 2, "selected cache writer plus its constant");

        let mut scratch = super::super::Scratch::acquire(1, 4, 8, None);
        let actual = super::super::Field::new(
            p.graph(),
            super::super::Geom {
                cell_width: 4,
                cell_height: 8,
            },
            &mut scratch,
        )
        .eval::<true>(p.root(), 3, 5, 7);
        assert_eq!(actual.to_bits(), 2.0f64.to_bits());
        assert_eq!(d.compute(crate::density::Context::new(3, 5, 7)).to_bits(), actual.to_bits());
        scratch.release();

        let interval = Density::IntervalSelect {
            input: b(Density::Const(2.0)),
            thresholds: vec![0.0, 1.0],
            functions: vec![
                Density::Interpolated {
                    inner: b(Density::Const(7.0)),
                    slot: 1,
                },
                Density::Const(8.0),
                Density::Const(9.0),
            ],
        };
        let interval_program = Program::compile(&interval);
        assert_eq!(interval_program.node_count(), 1);
        assert_eq!(interval_program.graph().op(interval_program.root()).kind, OpKind::Const);
        assert_eq!(
            interval_program.graph().params()[interval_program.graph().op(interval_program.root()).a as usize].to_bits(),
            9.0f64.to_bits()
        );
    }

    #[test]
    fn identical_selector_branches_drop_only_a_side_effect_free_input() {
        let no_writer = Density::RangeChoice {
            input: b(varying()),
            min_inclusive: 0.0,
            max_exclusive: 1.0,
            when_in_range: b(varying_alt()),
            when_out_of_range: b(varying_alt()),
        };
        let p = Program::compile(&no_writer);
        assert_eq!(p.graph().op(p.root()).kind, OpKind::YClampedGradient);

        let writer = Density::RangeChoice {
            input: b(Density::Interpolated {
                inner: b(Density::Const(1.0)),
                slot: 0,
            }),
            min_inclusive: 0.0,
            max_exclusive: 2.0,
            when_in_range: b(Density::Const(4.0)),
            when_out_of_range: b(Density::Const(4.0)),
        };
        let q = Program::compile(&writer);
        assert_eq!(q.graph().op(q.root()).kind, OpKind::RangeChoice);
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
            .eval::<true>(p.root(), x, y, z);
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
        // The transparent cache wrapper is omitted; the bare occurrence shares
        // the end-islands node with the wrapped occurrence.
        assert_eq!(p.node_count(), 2);
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
            input: b(varying()),
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
        .eval::<true>(p.root(), 1, 2, 3);
        assert_eq!(
            flat.to_bits(),
            d.compute(crate::density::Context::new(1, 2, 3)).to_bits()
        );
        scratch.release();
    }

    #[test]
    #[ignore = "local density evaluator instruction control"]
    fn density_eval_instruction_control() {
        use std::hint::black_box;

        let (noise, _) = twin_noises();
        let mut density = Density::Noise {
            noise,
            xz_scale: 0.03125,
            y_scale: 0.0625,
        };
        for i in 0..24 {
            density = Density::Add(
                b(density),
                b(Density::Squeeze(b(Density::Const(0.03125 * f64::from(i))))),
            );
        }
        let program = Program::compile(&density);
        let mut scratch = super::super::Scratch::acquire(0, 4, 8, None);
        for i in 0..128 {
            let x = i * 13 - 701;
            let y = i * 7 - 397;
            let z = i * 11 - 503;
            let actual = super::super::Field::new(
                program.graph(),
                super::super::Geom {
                    cell_width: 4,
                    cell_height: 8,
                },
                &mut scratch,
            )
            .eval::<true>(program.root(), x, y, z);
            let expected = density.compute(crate::density::Context::new(x, y, z));
            assert_eq!(actual.to_bits(), expected.to_bits(), "at ({x}, {y}, {z})");
        }

        let mut checksum = 0_u64;
        for i in 0..2_000_000_i32 {
            let x = i.wrapping_mul(13).wrapping_sub(701);
            let y = i.wrapping_mul(7).wrapping_sub(397);
            let z = i.wrapping_mul(11).wrapping_sub(503);
            checksum ^= black_box(
                super::super::Field::new(
                    program.graph(),
                    super::super::Geom {
                        cell_width: 4,
                        cell_height: 8,
                    },
                    &mut scratch,
                )
                .eval::<true>(program.root(), x, y, z)
                .to_bits(),
            );
        }
        println!("density eval instruction control: checksum={checksum}");
        scratch.release();
    }
}
