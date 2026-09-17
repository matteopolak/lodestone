//! Multi-noise overworld biome assignment.
//!
//! Vanilla assigns each column's biome by evaluating six climate density
//! functions (temperature, humidity, continentalness, erosion, depth,
//! weirdness — already computed by the same, already-JVM-proven [`Density`]
//! interpreter [`crate::overworld`] uses for shape) and finding the nearest
//! point, by squared distance, in a ~7.6k-row (parameter point, biome) table
//! (vanilla's own multi-noise biome parameter list). This module is that
//! search plus the
//! quantization glue; the table itself is **data**, not code (see below).
//!
//! # The table is bigger than expected — measured, not assumed
//!
//! The scoping plan estimated "~700 rows" from vanilla's own overworld biome
//! builder's
//! nested-loop structure. The oracle dump
//! (`scripts/worldgen-oracle/BiomeOracle.java`, mode `table`) says **7594**.
//! The finding that made this cheap still holds — no part of the 1124-line
//! builder needed transliterating, only its resolved output, reachable with
//! zero bootstrap via vanilla's own known-presets accessor —
//! but the row count itself was a guess from reading Java control flow, not a
//! measurement, and the guess was off by 10x.
//!
//! # Correction (Unit 9): "a brute-force search over 7594 points is trivial"
//!
//! This module used to conclude the paragraph above with "so this doesn't change
//! the cost story", and [`nearest_biome`] used to carry the same claim as a
//! reason for declining vanilla's own spatial-index search structure: *"a few thousand squared-distance
//! comparisons per quart column is already fast"*. **Both were true when written
//! and are false in composition**, which is why they survived review — nothing
//! about them looks stale.
//!
//! What changed underneath them is the *number of searches*, not their cost.
//! When they were written the only caller was [`crate::overworld`]'s per-quart
//! surface sample: 16 searches per chunk, ~121k comparisons, genuinely trivial.
//! Carver composition then added [`crate::overworld::OverworldGenerator`]'s
//! per-source-chunk resolution over a **17×17 = 289-chunk** neighbourhood, once
//! per pre-ore chunk, and a cold `column()` closes over 25 pre-ore chunks. That
//! is ~2.2M squared-distance comparisons per pre-ore chunk (`docs/plans/
//! worldgen-rewrite.md` D5, found by audit — no test could see it), and it is
//! overhead vanilla does not pay at all: vanilla ships **both** searches and
//! calls the tree.
//!
//! Unit 9 is the fix, in two independent halves: [`tree`] ports vanilla's
//! own spatial-index search structure (so a search stops being O(table_len)), and [`memo`]
//! memoises the per-source-chunk answer (so 289 searches per chunk become the
//! window's newly-entered strip).
//!
//! # And the tree is also the *answer* (the owner's R-tree ruling)
//!
//! The first landing of Unit 9 made the tree deliberately result-identical to
//! [`nearest_biome`], on the reasoning that the old engine was the JVM-proven
//! bridge. Measuring the two vanilla searches against each other then showed they
//! resolve to **different biome ids at 0.98% of arbitrary climate targets**, and
//! vanilla calls the tree. The owner ruled: do what vanilla does. So
//! [`BiomeTable::nearest_row`] now implements vanilla's own indexed search and
//! [`nearest_biome`] is the **documented divergence** rather than the target —
//! kept because it is the independent implementation proving the tree finds the
//! same minimum *distance* everywhere.
//!
//! The disagreement is exclusively about **which of several tied rows** to take;
//! neither search ever finds a different nearest *distance*. [`tree`]'s module doc
//! traces that, including the last-result carry-over now used by the production
//! lookup. The history is carried by an explicit generation cursor, while the
//! stateless indexed helper remains available as an explicit negative control.
//!
//! # The y = 0 trap
//!
//! An early version of this module's oracle fixtures sampled climate at a
//! fixed `y = 0` for every column, reasoning that `y = 0` is quart-aligned
//! (`0 % 4 == 0`) and simple. That produced almost exclusively cave and
//! deep-ocean biomes (`lush_caves`, `dripstone_caves`, `deep_dark`,
//! `deep_ocean`) at ordinary overworld surface coordinates — measured via
//! `BiomeOracle sample`, not assumed correct. The cause: `depth`'s density
//! function (`overworld/depth.json`) is `y_clamped_gradient(from_y: -64,
//! from_value: 1.5, to_y: 320, to_value: -1.5) + offset`, so at `y = 0` the
//! gradient term alone is already ≈ +1.0 — solidly in "deep underground"
//! climate-space, since vanilla's real per-quart 3-D biome assignment expects
//! `depth ≈ 0` only *near a column's own terrain surface*, not at a global
//! height. **This module samples at each quart's own generated surface
//! height** (the `heights[]` array [`crate::overworld::OverworldGenerator`]'s
//! fluid-fill stage already computes), not a constant — confirmed to recover
//! plausible surface biomes (plains, forest, savanna, beach, swamp, ocean
//! variants) at the same coordinates.
//!
//! # Resolution: one biome per quart column, not per quart cube
//!
//! Vanilla's real biome assignment is fully 3-D — every `(quartX, quartY,
//! quartZ)` gets its own sample, so a single `(x, z)` column can carry
//! different biomes at different depths (surface vs. a deep-dark cave
//! pocket). This port computes **one climate sample per horizontal quart
//! `(qx, qz)`** (16 per chunk, at that quart's own surface height) and
//! broadcasts it to every `y` in that quart column. This is deliberately
//! scoped to what Phase 1 needs to be observable and testable — "the biome a
//! player sees while exploring the surface" — and is *cheaper*, not just
//! simpler, than full 3-D: caves are not composed into
//! [`crate::overworld::OverworldGenerator`] yet (tracked as Phase 2), so
//! there is no cave volume for a vertically-varying biome to describe today.
//! Revisiting this is the natural first step of Phase 2, once caves exist to
//! carry `dripstone_caves`/`lush_caves`/`deep_dark` into.
//!
//! # Three biomes this port could not surface, until now
//!
//! `minecraft:badlands`, `minecraft:eroded_badlands` and
//! `minecraft:wooded_badlands` all reach vanilla's own bandlands surface rule in the
//! overworld surface rule tree (confirmed by walking the JSON: both
//! `bandlands` nodes sit under a `condition{biome_is:[badlands,
//! eroded_badlands, wooded_badlands]}` guard, nothing else). Vanilla's
//! `Bandlands` rule delegates to vanilla's own band-lookup routine —
//! **now ported**
//! (`crate::surface`'s `Rule::Bandlands`/`BandBlocks`/`generate_bands`, an
//! earlier carried-over gap): its own noise (`clay_bands_offset`) and the
//! banded-terracotta-column generator are reproduced from the documented
//! algorithm, checked against the running server.
//!
//! Before this module existed those three biomes were unreachable (the world
//! ran under a single fixed `minecraft:plains`), so `Rule::Bandlands`'s old
//! panic was dead code; once real biome variety landed, reaching it would
//! have crashed chunk generation the moment a player's world contained
//! badlands, so [`usable_overworld_table`] excluded exactly these three from
//! the searchable table as a deliberate, documented Phase 1 gap. That
//! exclusion is now removed — [`usable_overworld_table`] is a pass-through —
//! so the nearest-neighbour search can select any of the three again.
//! [`UNSUPPORTED_SURFACE_BIOMES`] itself is kept (not deleted): it is a
//! public item another crate imports by name (see its own doc comment).
//!
//! **Not ported by this increment**: vanilla's own eroded-badlands extension
//! (the separate stone-pillar height extension unconditionally applied to
//! every `eroded_badlands` column, unrelated to the band lookup's terracotta
//! banding) and vanilla's own frozen-ocean extension (a different biome pair entirely).
//! Neither is reachable through `Rule::Bandlands`, and un-filtering the
//! three names above does not require either — see
//! `docs/worldgen-parity.md` for what was and wasn't measured here.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::Value;
use lodestone_data::biomes::{BiomeRef, BuiltinBiome};

use crate::density::{Builder, Context, Density, Resolver};

pub(crate) mod memo;
mod tree;

/// The three biomes [`usable_overworld_table`] used to exclude before
/// vanilla's own band lookup was ported — see the module doc's "Three biomes
/// this port could not surface, until now" section. The name is now a
/// historical artifact, not a current filter: kept (rather than deleted or
/// renamed) because `lodestone_server::worldgen_data`'s
/// `served_columns_never_carry_an_unported_badlands_variant` test imports it
/// by this name, and that crate is outside this session's edit scope (see
/// this crate's own `CLAUDE.md` file-ownership note) — that test's own
/// premise is now stale and needs an update in its owning crate, not
/// something fixable from here.
pub const UNSUPPORTED_SURFACE_BIOMES: [&str; 3] = [
    "minecraft:badlands",
    "minecraft:eroded_badlands",
    "minecraft:wooded_badlands",
];

/// One climate axis's quantized `[min, max]` span — vanilla's own climate
/// parameter's
/// internal representation (`(coord * 10000.0f) as i64`, already applied
/// before this type is ever constructed; see [`quantize_coord`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parameter {
    pub min: i64,
    pub max: i64,
}

impl Parameter {
    /// Vanilla's own climate-parameter distance calculation.
    #[must_use]
    fn distance(&self, target: i64) -> i64 {
        let above = target - self.max;
        let below = self.min - target;
        if above > 0 { above } else { below.max(0) }
    }
}

/// A biome's climate cell: 7 quantized spans in vanilla's fixed axis order
/// (temperature, humidity, continentalness, erosion, depth, weirdness,
/// offset). `offset` is stored as a degenerate `[o, o]` span so it folds into
/// the same generic distance formula as the other six — exactly what
/// vanilla's own climate parameter-point space construction does internally (it appends
/// `Parameter(offset, offset)` as element 7 before handing the array to the
/// spatial-index/brute-force search), so this is not a simplification, it is the
/// same representation vanilla uses.
#[derive(Debug, Clone)]
pub struct BiomeParameterPoint {
    /// `[temperature, humidity, continentalness, erosion, depth, weirdness, offset]`.
    pub params: [Parameter; 7],
    pub biome: String,
}

impl BiomeParameterPoint {
    /// Vanilla's own climate parameter-point fitness calculation against a target point. `target`'s 7th slot
    /// (offset) is always `0` for a real climate sample — only a biome's own
    /// parameter point ever carries a nonzero offset span.
    #[must_use]
    pub fn fitness(&self, target: &[i64; 7]) -> i64 {
        let mut sum = 0i64;
        for i in 0..7 {
            let d = self.params[i].distance(target[i]);
            sum += d * d;
        }
        sum
    }
}

/// Quantizes a climate coordinate exactly as vanilla's own coordinate
/// quantization does:
/// `(long)(coord * 10000.0F)`. The multiplication happens in **`f32`**
/// precision in vanilla (the value is cast to `float` before this point, in
/// its climate sampler's own per-axis compute-then-cast-to-float step), so
/// this casts to `f32` first — not a `f64` quantization rounded afterward —
/// to reproduce the exact same truncation vanilla gets.
#[must_use]
pub fn quantize_coord(v: f64) -> i64 {
    ((v as f32) * 10000.0_f32) as i64
}

/// The table row of the nearest biome by squared climate distance —
/// vanilla's own brute-force parameter-list search — its
/// own un-optimized reference search, sitting next to the R-tree it also ships.
///
/// **Not the production path, and since the R-tree ruling not the target either.** Production
/// goes through [`BiomeTable::nearest_row`], which reproduces vanilla's own
/// indexed search — and vanilla calls the tree, so the tree is the answer. This
/// stays as the independent implementation that proves the tree finds the same
/// minimum *distance* at every target, and as the **documented divergence**: it
/// breaks a distance tie by earliest table row where vanilla's tree breaks it by
/// traversal order, which resolves to a different biome id at 0.98% of arbitrary
/// targets. See [`tree`]'s module doc and `tests/biome_tree_identity.rs`.
///
/// It bumps no counter, so a gate may call it millions of times without polluting
/// the measurement the tree is judged by.
///
/// Matches vanilla's tie-break exactly: ties keep the **earlier** table entry
/// (a strict `<` comparison against the best fitness so far, never `<=`), so `table`'s order must match the
/// oracle dump's order — [`parse_table`] preserves JSON array order for exactly
/// this reason.
///
/// # Panics
/// Panics if `table` is empty.
#[must_use]
pub fn nearest_row_brute_force(table: &[BiomeParameterPoint], target: &[i64; 7]) -> u32 {
    let mut best_row = 0u32;
    let mut best_fitness = table[0].fitness(target);
    for (row, entry) in table.iter().enumerate().skip(1) {
        let fitness = entry.fitness(target);
        if fitness < best_fitness {
            best_fitness = fitness;
            best_row = row as u32;
        }
    }
    best_row
}

/// [`nearest_row_brute_force`]'s answer as a biome id. Kept as a public name
/// because it predates U9 and reads as the obvious entry point; production uses
/// [`BiomeTable::nearest`] instead.
///
/// # Panics
/// Panics if `table` is empty.
#[must_use]
pub fn nearest_biome<'a>(table: &'a [BiomeParameterPoint], target: &[i64; 7]) -> &'a str {
    // Diagnostic D5. Both numbers matter and neither implies the other: the
    // search *count* is what U9's memoisation reduces, while rows examined per
    // search is what the RTree port reduces. A single "searches" counter would
    // make an RTree that searched just as often look like no improvement.
    crate::counters::bump_biome_search(table.len() as u64);
    &table[nearest_row_brute_force(table, target) as usize].biome
}

/// Monotonic id source for [`BiomeTable::id`] — see [`memo`] for why the memo's
/// key has to carry table identity as well as chunk position. Starts at 1 so
/// zero can be [`memo`]'s empty-slot sentinel.
static NEXT: AtomicU64 = AtomicU64::new(1);

fn next_table_id() -> u64 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// The parsed climate table plus its search structure: a [`Vec`] of
/// [`BiomeParameterPoint`] in the oracle dump's row order, and the
/// [`tree::BiomeTree`] built from it.
///
/// # Why this type exists at all
///
/// The search structure has to be built once per generator and live beside the
/// rows it indexes, and the rows are reached today as
/// `OverworldGenerator::dynamic_biome`'s `table` field — whose struct literal
/// lives in `overworld/mod.rs`, one of this repo's six measured choke-point files
/// (`CLAUDE.md`; `docs/worldgen-staged-store.md` owns that file for Unit 6).
/// Returning this from [`usable_overworld_table`] instead of a bare `Vec` puts
/// the tree in place with **no edit to any file outside `src/biome/` and
/// `src/overworld/biome.rs`**, because the [`Deref`](std::ops::Deref) and
/// [`IntoIterator`] impls below keep every existing `Vec`-shaped use compiling
/// unchanged — `d.table.iter()` in `overworld/mod.rs` and
/// `table.into_iter().map(|p| p.biome)` in `lodestone_server::worldgen_data`.
///
/// That is a deliberate trade and worth naming: a `Deref` to a slice hides that
/// the type carries more than the slice. The alternative was a one-line patch to
/// a file two other units were mid-flight in.
#[derive(Clone)]
struct BiomeTableData {
    points: Arc<Vec<BiomeParameterPoint>>,
    tree: Arc<tree::BiomeTree>,
    /// Built-in identity for each row when the resource names resolve. A
    /// `None` entry is retained for small synthetic tables used by search
    /// tests; production resource loading uses [`Self::new_strict`].
    builtin_biomes: Arc<Vec<Option<BuiltinBiome>>>,
}

#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub struct BiomeTable {
    data: Arc<BiomeTableData>,
    id: u64,
}

static BIOME_TABLE_CACHE: OnceLock<Mutex<HashMap<u64, Arc<BiomeTableData>>>> = OnceLock::new();

/// Explicit history for one deterministic biome-search lifecycle. The cursor
/// is owned by its generation unit and is never inferred from worker identity.
#[derive(Debug, Clone, Copy)]
pub struct BiomeSearchCursor {
    table_id: u64,
    leaf: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CachedBiomeAnswer {
    row: u32,
    leaf: u32,
    distance: i64,
}

impl BiomeTable {
    /// Builds the search structure over `points`, preserving row order.
    ///
    /// # Panics
    /// Panics if `points` is empty — [`crate::overworld::OverworldGenerator`]
    /// only constructs one when the resolver supplied a non-empty table.
    #[must_use]
    pub fn new(points: Vec<BiomeParameterPoint>) -> Self {
        let builtin_biomes = points
            .iter()
            .map(|point| BuiltinBiome::from_name(&point.biome))
            .collect();
        Self::from_data(Arc::new(BiomeTableData {
            tree: Arc::new(tree::BiomeTree::build(&points)),
            points: Arc::new(points),
            builtin_biomes: Arc::new(builtin_biomes),
        }))
    }

    fn from_data(data: Arc<BiomeTableData>) -> Self {
        Self {
            data,
            id: next_table_id(),
        }
    }

    fn new_cached(points: Vec<BiomeParameterPoint>, fingerprint: u64) -> Self {
        let cache = BIOME_TABLE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        if let Some(data) = cache
            .lock()
            .expect("biome table cache lock poisoned")
            .get(&fingerprint)
            .cloned()
        {
            return Self::from_data(data);
        }
        let builtin_biomes = points
            .iter()
            .map(|point| BuiltinBiome::from_name(&point.biome))
            .collect();
        let data = Arc::new(BiomeTableData {
            tree: Arc::new(tree::BiomeTree::build(&points)),
            points: Arc::new(points),
            builtin_biomes: Arc::new(builtin_biomes),
        });
        let mut entries = cache.lock().expect("biome table cache lock poisoned");
        if let Some(existing) = entries.get(&fingerprint).cloned() {
            return Self::from_data(existing);
        }
        entries.insert(fingerprint, Arc::clone(&data));
        Self::from_data(data)
    }

    /// Builds a table at a resource-loading boundary and rejects any name that
    /// is not one of this build's generated built-in biomes. Dynamic entries
    /// must be admitted by an explicit extension registry before they reach a
    /// worldgen table; they are never silently coerced into a built-in.
    #[must_use]
    pub fn new_strict(points: Vec<BiomeParameterPoint>) -> Self {
        let table = Self::new(points);
        table.assert_strict();
        table
    }

    fn new_cached_strict(points: Vec<BiomeParameterPoint>, fingerprint: u64) -> Self {
        let table = Self::new_cached(points, fingerprint);
        table.assert_strict();
        table
    }

    fn assert_strict(&self) {
        if let Some((row, _)) = self
            .data
            .builtin_biomes
            .iter()
            .enumerate()
            .find(|(_, biome)| biome.is_none())
        {
            panic!(
                "unknown built-in biome {:?} at parameter row {row}",
                self.data.points[row].biome
            );
        }
    }

    /// The stateless indexed answer. Production generation uses
    /// [`Self::nearest_row_with_cursor`] with an explicit lifecycle cursor.
    #[must_use]
    pub fn nearest_row(&self, target: &[i64; 7]) -> u32 {
        self.data.tree.nearest_row(target)
    }

    /// The fresh-instance indexed answer, with no cached history. This is the
    /// negative control for parity fixtures and diagnostics; production callers
    /// use [`Self::nearest_row`] so query history is preserved.
    #[must_use]
    pub fn nearest_row_stateless(&self, target: &[i64; 7]) -> u32 {
        self.data.tree.nearest_row(target)
    }

    /// Starts a fresh indexed-search lifecycle owned by the caller.
    #[must_use]
    pub fn search_cursor(&self) -> BiomeSearchCursor {
        BiomeSearchCursor {
            table_id: self.id,
            leaf: None,
        }
    }

    /// The indexed answer using and updating an explicit lifecycle cursor.
    #[must_use]
    pub fn nearest_row_with_cursor(
        &self,
        target: &[i64; 7],
        cursor: &mut BiomeSearchCursor,
    ) -> u32 {
        assert_eq!(cursor.table_id, self.id, "biome cursor belongs to another table");
        let (row, leaf) = self.data.tree.nearest_row_with_candidate(target, cursor.leaf);
        cursor.leaf = Some(leaf);
        row
    }

    pub(crate) fn cached_answer(&self, target: &[i64; 7]) -> CachedBiomeAnswer {
        let (row, leaf, distance) = self.data.tree.nearest_leaf_and_distance(target);
        CachedBiomeAnswer {
            row,
            leaf,
            distance,
        }
    }

    pub(crate) fn apply_cached_answer(
        &self,
        target: &[i64; 7],
        cursor: &mut BiomeSearchCursor,
        answer: CachedBiomeAnswer,
    ) -> u32 {
        assert_eq!(cursor.table_id, self.id, "biome cursor belongs to another table");
        if let Some(leaf) = cursor.leaf {
            let (row, distance) = self.data.tree.leaf_row_and_distance(leaf, target);
            if distance == answer.distance {
                return row;
            }
        }
        cursor.leaf = Some(answer.leaf);
        answer.row
    }

    /// Updates a cursor from a previously memoised row without re-running the
    /// search. This preserves the reference lookup history when a source-chunk
    /// memo supplies the same row on a later query.
    pub fn cursor_from_row(&self, cursor: &mut BiomeSearchCursor, row: u32) {
        assert_eq!(cursor.table_id, self.id, "biome cursor belongs to another table");
        cursor.leaf = self.data.tree.leaf_node_for_row(row);
    }

    /// The nearest biome's id via a fresh indexed lookup. Production pipelines
    /// use [`Self::nearest_row_with_cursor`] so tie history is explicit.
    #[must_use]
    pub fn nearest(&self, target: &[i64; 7]) -> &str {
        &self.data.points[self.nearest_row(target) as usize].biome
    }

    /// The biome id at a row, for a caller holding a memoised row.
    ///
    /// # Panics
    /// Panics if `row` is out of range, which can only happen if a row from a
    /// *different* table reached here — the bug [`memo`]'s `table_id` tag exists
    /// to make impossible.
    #[must_use]
    pub fn biome_at(&self, row: u32) -> &str {
        &self.data.points[row as usize].biome
    }

    /// The compact typed identity at a row, if that row names a generated
    /// built-in. The `None` case is intentional for synthetic tables; strict
    /// production tables reject it during construction.
    #[must_use]
    pub fn biome_ref_at(&self, row: u32) -> Option<BiomeRef> {
        self.data
            .builtin_biomes
            .get(row as usize)
            .and_then(|biome| biome.map(BiomeRef::builtin))
    }

    /// This table's memo identity — see [`memo`].
    #[must_use]
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Every `(parent, child, axis)` violating the tree's hull-containment
    /// premise; empty for a correctly built tree. Exposed for
    /// `tests/biome_tree_identity.rs` — see [`tree::BiomeTree::hull_containment_violations`].
    #[must_use]
    pub fn hull_containment_violations(&self) -> Vec<(u32, u32, usize)> {
        self.data.tree.hull_containment_violations()
    }

    /// `(total nodes, leaves)` in the tree. Leaves must equal
    /// [`BiomeTable::len`].
    #[must_use]
    pub fn tree_shape(&self) -> (usize, usize) {
        self.data.tree.shape()
    }

    /// Node count, for a gate that perturbs one by index.
    #[must_use]
    pub fn tree_node_count(&self) -> usize {
        self.data.tree.node_count()
    }

    /// Whether tree node `id` is a leaf. Gate support: a control that perturbs a
    /// *leaf* cannot break the pruning bound (a narrower child is still contained
    /// by its parent), so a control has to target an interior node and prove it
    /// targeted one.
    ///
    /// Nodes are laid out in DFS **pre-order**, so node 0 is the root and node 1
    /// is its first child.
    #[must_use]
    pub fn tree_node_is_leaf(&self, id: usize) -> bool {
        self.data.tree.node_is_leaf(id)
    }

    /// Collapses one tree node's span to a point, breaking the lower-bound
    /// premise the pruning relies on. **The control** for the hull-containment and
    /// distance-identity gates: an assertion that cannot fail under this is not
    /// evidence.
    pub fn perturb_tree_node(&mut self, node: usize) {
        Arc::make_mut(&mut Arc::make_mut(&mut self.data).tree).perturb_node_span(node);
    }

    /// `(row, squared distance)` of the selected leaf. Exposed because since the R-tree ruling
    /// the *distance* claim and the *row* claim have different strengths: the
    /// distance always matches [`nearest_row_brute_force`], the row matches it only
    /// where no tie exists. See [`tree`]'s module doc.
    #[must_use]
    pub fn nearest_row_and_distance(&self, target: &[i64; 7]) -> (u32, i64) {
        self.data.tree.nearest_row_and_distance(target)
    }

    /// Indexed search with an explicit candidate, exposed for tie controls and
    /// tests. Production uses the per-worker history in [`Self::nearest_row`].
    #[must_use]
    pub fn nearest_row_seeded(&self, target: &[i64; 7], candidate: Option<u32>) -> u32 {
        self.data.tree.nearest_row_seeded(target, candidate)
    }

    /// The tree node id of the leaf carrying `row`, so a gate can seed a search
    /// with a chosen row.
    #[must_use]
    pub fn leaf_node_for_row(&self, row: u32) -> Option<u32> {
        self.data.tree.leaf_node_for_row(row)
    }

    /// The root's child node ids in traversal order — see
    /// [`tree::BiomeTree::root_child_nodes`] for why a control has to perturb a
    /// *later* one than the first.
    #[must_use]
    pub fn tree_root_child_nodes(&self) -> Vec<u32> {
        self.data.tree.root_child_nodes()
    }
}

impl std::ops::Deref for BiomeTable {
    type Target = [BiomeParameterPoint];

    fn deref(&self) -> &Self::Target {
        self.data.points.as_slice()
    }
}

impl IntoIterator for BiomeTable {
    type Item = BiomeParameterPoint;
    type IntoIter = std::vec::IntoIter<BiomeParameterPoint>;

    fn into_iter(self) -> Self::IntoIter {
        match Arc::try_unwrap(self.data) {
            Ok(data) => Arc::try_unwrap(data.points)
                .unwrap_or_else(|points| points.as_ref().clone())
                .into_iter(),
            Err(data) => data.points.as_ref().clone().into_iter(),
        }
    }
}

/// Parses the embedded overworld biome-parameter table dumped by
/// `BiomeOracle`'s `table` mode. Schema: a JSON array of 14-element rows,
/// `[tMin,tMax,hMin,hMax,cMin,cMax,eMin,eMax,dMin,dMax,wMin,wMax,offset,"biome"]`
/// — the same order [`BiomeParameterPoint::params`] uses, and the raw
/// quantized `long`s vanilla's own climate parameter carries internally (not
/// re-derived from decimal floats), so parsing never round-trips through a
/// second float parse.
///
/// # Panics
/// Panics on any row that isn't exactly 13 numbers followed by a string.
#[must_use]
pub fn parse_table(value: &Value) -> Vec<BiomeParameterPoint> {
    value
        .as_array()
        .expect("biome parameter table must be a JSON array")
        .iter()
        .map(|row| {
            let row = row.as_array().expect("biome parameter row must be an array");
            assert_eq!(
                row.len(),
                14,
                "biome parameter row must have 13 numbers + 1 biome name, got {}",
                row.len()
            );
            let n = |i: usize| {
                row[i]
                    .as_i64()
                    .unwrap_or_else(|| panic!("biome parameter row[{i}] is not an integer"))
            };
            let point = |lo: usize, hi: usize| Parameter {
                min: n(lo),
                max: n(hi),
            };
            let offset = n(12);
            let params = [
                point(0, 1),
                point(2, 3),
                point(4, 5),
                point(6, 7),
                point(8, 9),
                point(10, 11),
                Parameter {
                    min: offset,
                    max: offset,
                },
            ];
            let biome = row[13]
                .as_str()
                .expect("biome parameter row[13] must be a biome id string")
                .to_string();
            BiomeParameterPoint { params, biome }
        })
        .collect()
}

/// Used to drop [`UNSUPPORTED_SURFACE_BIOMES`] from a parsed table before
/// vanilla's own band lookup was ported (`crate::surface::Rule::Bandlands`) —
/// see the module doc's "Three biomes this port could not surface, until
/// now" section. Still a pass-through in the sense that matters: every biome in
/// the parsed table, including the three formerly-excluded badlands variants, is
/// searchable, and row order is preserved.
///
/// Since Unit 9 it also **builds the search tree**, and is therefore the seam
/// that put [`BiomeTable`] in place without touching `overworld/mod.rs` — see
/// [`BiomeTable`]'s own doc for that reasoning. Callers that only wanted the rows
/// need no change: [`BiomeTable`] derefs to `[BiomeParameterPoint]` and consumes
/// into an iterator of them.
#[must_use]
pub fn usable_overworld_table(table: Vec<BiomeParameterPoint>) -> BiomeTable {
    BiomeTable::new_strict(table)
}

/// Loads the overworld table, reusing its immutable parsed catalog only when the
/// resolver identifies a stable asset bundle. Resolvers without a fingerprint
/// remain fully uncached.
#[must_use]
pub fn overworld_table(resolver: &dyn Resolver) -> Option<BiomeTable> {
    let fingerprint = resolver.asset_fingerprint();
    if let Some(fingerprint) = fingerprint {
        let cache = BIOME_TABLE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        if let Some(data) = cache
            .lock()
            .expect("biome table cache lock poisoned")
            .get(&fingerprint)
            .cloned()
        {
            return Some(BiomeTable::from_data(data));
        }
    }
    let points = parse_table(&resolver.biome_parameters());
    if points.is_empty() {
        return None;
    }
    Some(match fingerprint {
        Some(fingerprint) => BiomeTable::new_cached_strict(points, fingerprint),
        None => BiomeTable::new_strict(points),
    })
}

/// Parses the embedded per-biome `temperature` map (`{"minecraft:plains":
/// 0.8, ...}`) from the authoritative generated biome JSON data. This field
/// is static data and does not require runtime evaluation.
///
/// # Panics
/// Panics if `value` is not a JSON object of biome-id -> number.
#[must_use]
pub fn parse_temperatures(value: &Value) -> HashMap<String, f32> {
    value
        .as_object()
        .expect("biome temperature table must be a JSON object")
        .iter()
        .map(|(k, v)| {
            let t = v
                .as_f64()
                .unwrap_or_else(|| panic!("biome temperature for {k} is not a number"))
                as f32;
            (k.clone(), t)
        })
        .collect()
}

/// Approximates vanilla's own "warm enough to rain"/"cold enough to snow"'s `< 0.15`
/// threshold from the biome's *declared* `temperature` field, ignoring the
/// per-block height adjustment (vanilla's own height-adjusted-temperature
/// calculation, a noise
/// + `(y - sea_level - 17) * 0.05/40` correction above `sea_level + 17`) and any
/// `temperature_modifier` (e.g. `frozen`, which lowers the effective value
/// for a handful of ocean biomes). This is not a new simplification: before
/// this module existed, `cold_enough_to_snow` was already a single fixed
/// bool for the whole world (`worldgen_data::DEFAULT_BIOME_SNOWS`); this just
/// computes that same kind of answer per selected biome instead of once
/// globally. Revisiting the height adjustment is a small, independent
/// follow-up if a snow-line seam near `sea_level + 17` ever needs it.
#[must_use]
pub fn cold_enough_to_snow(temperatures: &HashMap<String, f32>, biome: &str) -> bool {
    temperatures.get(biome).is_none_or(|&t| t < 0.15)
}

/// Evaluates the six named climate channels (`noise_router.{temperature,
/// vegetation, continents, erosion, depth, ridges}` — `vegetation` is
/// vanilla's field name for humidity, `ridges` for weirdness) at a block
/// position, quantizing exactly as vanilla's own climate sampling and target
/// derivation do. Built once per generator (like [`crate::overworld::OverworldGenerator`]'s
/// `final_density`), reusing the same [`Density`] interpreter that
/// `region_parity`'s whole-region test already proves bit-exact against the
/// JVM for these exact six outputs (`RegionOracle.java` dumps
/// `continents`/`erosion`/`ridges`/`temperature`/`vegetation`/`depth`
/// directly) — so nothing new needs re-verifying here except the
/// quantization and the search.
#[allow(missing_debug_implementations)]
pub struct ClimateSampler {
    temperature: Density,
    humidity: Density,
    continentalness: Density,
    erosion: Density,
    depth: Density,
    weirdness: Density,
    xz_pure: [bool; 6],
}

/// Climate values that are invariant across Y. Callers size this to the exact
/// quart X/Z footprint they own; it is never shared across requests.
pub(crate) struct PreparedClimateGrid {
    min_qx: i32,
    min_qz: i32,
    width: usize,
    depth: usize,
    values: Vec<[i64; 6]>,
}

#[derive(Clone, Copy)]
pub(crate) struct PreparedClimateGridView<'a> {
    grid: &'a PreparedClimateGrid,
    min_qx: i32,
    min_qz: i32,
    width: usize,
    depth: usize,
}

impl PreparedClimateGrid {
    pub(crate) fn full_view(&self) -> PreparedClimateGridView<'_> {
        self.view(self.min_qx, self.min_qz, self.width, self.depth)
    }

    pub(crate) fn center_view(&self) -> PreparedClimateGridView<'_> {
        assert!(self.width >= 6 && self.depth >= 6, "climate grid has no center view");
        self.view(self.min_qx + 1, self.min_qz + 1, 4, 4)
    }

    pub(crate) fn view(
        &self,
        min_qx: i32,
        min_qz: i32,
        width: usize,
        depth: usize,
    ) -> PreparedClimateGridView<'_> {
        assert!(width > 0 && depth > 0, "climate grid view must be non-empty");
        let max_qx = min_qx
            .checked_add(i32::try_from(width - 1).expect("climate grid view width"))
            .expect("climate grid view x overflow");
        let max_qz = min_qz
            .checked_add(i32::try_from(depth - 1).expect("climate grid view depth"))
            .expect("climate grid view z overflow");
        let grid_max_qx = self
            .min_qx
            .checked_add(i32::try_from(self.width - 1).expect("climate grid width"))
            .expect("climate grid x overflow");
        let grid_max_qz = self
            .min_qz
            .checked_add(i32::try_from(self.depth - 1).expect("climate grid depth"))
            .expect("climate grid z overflow");
        assert!(
            self.min_qx <= min_qx
                && max_qx <= grid_max_qx
                && self.min_qz <= min_qz
                && max_qz <= grid_max_qz,
            "climate grid view escaped its prepared bounds"
        );
        PreparedClimateGridView {
            grid: self,
            min_qx,
            min_qz,
            width,
            depth,
        }
    }
}

impl PreparedClimateGridView<'_> {
    #[inline]
    fn values_at(&self, x: i32, z: i32) -> Option<&[i64; 6]> {
        if x.rem_euclid(4) != 0 || z.rem_euclid(4) != 0 {
            return None;
        }
        let qx = x.div_euclid(4);
        let qz = z.div_euclid(4);
        let local_qx = qx.checked_sub(self.min_qx)?;
        let local_qz = qz.checked_sub(self.min_qz)?;
        if local_qx < 0
            || local_qz < 0
            || local_qx >= i32::try_from(self.width).ok()?
            || local_qz >= i32::try_from(self.depth).ok()?
        {
            return None;
        }
        let root_qx = self.min_qx.checked_add(local_qx)?;
        let root_qz = self.min_qz.checked_add(local_qz)?;
        let root_x = root_qx.checked_sub(self.grid.min_qx)?;
        let root_z = root_qz.checked_sub(self.grid.min_qz)?;
        if root_x < 0
            || root_z < 0
            || root_x >= i32::try_from(self.grid.width).ok()?
            || root_z >= i32::try_from(self.grid.depth).ok()?
        {
            return None;
        }
        Some(&self.grid.values[root_z as usize * self.grid.width + root_x as usize])
    }
}

impl ClimateSampler {
    #[must_use]
    pub fn new(settings: &Value, builder: &Builder) -> Self {
        let router = &settings["noise_router"];
        let sampler = Self {
            temperature: builder
                .build(&router["temperature"])
                .expect("bundled temperature density-function document"),
            humidity: builder
                .build(&router["vegetation"])
                .expect("bundled vegetation density-function document"),
            continentalness: builder
                .build(&router["continents"])
                .expect("bundled continents density-function document"),
            erosion: builder
                .build(&router["erosion"])
                .expect("bundled erosion density-function document"),
            depth: builder
                .build(&router["depth"])
                .expect("bundled depth density-function document"),
            weirdness: builder
                .build(&router["ridges"])
                .expect("bundled ridges density-function document"),
            xz_pure: [false; 6],
        };
        sampler.with_xz_purity()
    }

    fn with_xz_purity(mut self) -> Self {
        self.xz_pure = [
            self.temperature.is_xz_pure(),
            self.humidity.is_xz_pure(),
            self.continentalness.is_xz_pure(),
            self.erosion.is_xz_pure(),
            self.depth.is_xz_pure(),
            self.weirdness.is_xz_pure(),
        ];
        self
    }

    /// Vanilla's own climate sampler's quantized target, at an exact block
    /// position (the caller is responsible for quart-aligning `x`/`z` and
    /// picking `y`; see the module doc's "y = 0 trap" section for why `y`
    /// must be the column's own surface height, not a constant). The 7th
    /// slot (offset) is always `0`: a *target* point never carries an
    /// offset, only a biome's own [`BiomeParameterPoint`] does.
    #[must_use]
    pub fn target(&self, x: i32, y: i32, z: i32) -> [i64; 7] {
        let ctx = Context::new(x, y, z);
        [
            quantize_coord(self.temperature.compute(ctx)),
            quantize_coord(self.humidity.compute(ctx)),
            quantize_coord(self.continentalness.compute(ctx)),
            quantize_coord(self.erosion.compute(ctx)),
            quantize_coord(self.depth.compute(ctx)),
            quantize_coord(self.weirdness.compute(ctx)),
            0,
        ]
    }

    /// Prepares the Y-invariant channels for one column's sixteen quart
    /// positions. A channel is admitted only when the density tree proves its
    /// value is independent of Y.
    #[cfg(test)]
    #[inline]
    pub(crate) fn prepare_xz(&self, base_x: i32, base_z: i32) -> PreparedClimateGrid {
        self.prepare_xz_rect(
            base_x.div_euclid(4),
            base_z.div_euclid(4),
            4,
            4,
        )
    }

    pub(crate) fn prepare_xz_rect(
        &self,
        min_qx: i32,
        min_qz: i32,
        width: usize,
        depth: usize,
    ) -> PreparedClimateGrid {
        let mut values = vec![[0; 6]; width * depth];
        for qz in 0..depth {
            for qx in 0..width {
                let index = qz * width + qx;
                let ctx = Context::new((min_qx + qx as i32) * 4, 0, (min_qz + qz as i32) * 4);
                for (channel, value) in values[index].iter_mut().enumerate() {
                    if self.xz_pure[channel] {
                        *value = quantize_coord(self.channel(channel).compute(ctx));
                    }
                }
            }
        }
        PreparedClimateGrid {
            min_qx,
            min_qz,
            width,
            depth,
            values,
        }
    }

    /// Computes a target using the prepared X/Z channels and the exact query Y
    /// for every channel that was not proven invariant.
    #[inline]
    pub(crate) fn target_prepared(
        &self,
        prepared: &PreparedClimateGrid,
        x: i32,
        y: i32,
        z: i32,
    ) -> [i64; 7] {
        self.target_prepared_view(&prepared.full_view(), x, y, z)
    }

    pub(crate) fn target_prepared_view(
        &self,
        prepared: &PreparedClimateGridView<'_>,
        x: i32,
        y: i32,
        z: i32,
    ) -> [i64; 7] {
        let values = prepared.values_at(x, z).unwrap_or_else(|| {
            panic!("prepared climate lookup escaped its view at ({x}, {z})")
        });
        let ctx = Context::new(x, y, z);
        [
            if self.xz_pure[0] {
                values[0]
            } else {
                quantize_coord(self.temperature.compute(ctx))
            },
            if self.xz_pure[1] {
                values[1]
            } else {
                quantize_coord(self.humidity.compute(ctx))
            },
            if self.xz_pure[2] {
                values[2]
            } else {
                quantize_coord(self.continentalness.compute(ctx))
            },
            if self.xz_pure[3] {
                values[3]
            } else {
                quantize_coord(self.erosion.compute(ctx))
            },
            if self.xz_pure[4] {
                values[4]
            } else {
                quantize_coord(self.depth.compute(ctx))
            },
            if self.xz_pure[5] {
                values[5]
            } else {
                quantize_coord(self.weirdness.compute(ctx))
            },
            0,
        ]
    }

    fn channel(&self, channel: usize) -> &Density {
        match channel {
            0 => &self.temperature,
            1 => &self.humidity,
            2 => &self.continentalness,
            3 => &self.erosion,
            4 => &self.depth,
            5 => &self.weirdness,
            _ => unreachable!("climate channel index"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::density::NoiseParams;
    use std::path::{Path, PathBuf};

    struct SupportResolver {
        root: PathBuf,
    }

    impl SupportResolver {
        fn read(&self, kind: &str, id: &str) -> Value {
            let name = id.strip_prefix("minecraft:").unwrap_or(id);
            let path = self.root.join(kind).join(format!("{name}.json"));
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
            serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display()))
        }
    }

    impl Resolver for SupportResolver {
        fn density_function(&self, id: &str) -> Value {
            self.read("density_function", id)
        }

        fn noise(&self, id: &str) -> NoiseParams {
            let value = self.read("noise", id);
            NoiseParams {
                first_octave: value["firstOctave"]
                    .as_i64()
                    .expect("firstOctave") as i32,
                amplitudes: value["amplitudes"]
                    .as_array()
                    .expect("amplitudes")
                    .iter()
                    .map(|amplitude| amplitude.as_f64().expect("amplitude"))
                    .collect(),
            }
        }
    }

    fn tiny_table() -> Vec<BiomeParameterPoint> {
        // Two points on the temperature axis only, everything else spans the
        // full [-10000, 10000] range so only temperature discriminates.
        let full = || Parameter {
            min: -10000,
            max: 10000,
        };
        vec![
            BiomeParameterPoint {
                params: [
                    Parameter {
                        min: -10000,
                        max: -5000,
                    },
                    full(),
                    full(),
                    full(),
                    full(),
                    full(),
                    Parameter { min: 0, max: 0 },
                ],
                biome: "minecraft:cold".to_string(),
            },
            BiomeParameterPoint {
                params: [
                    Parameter {
                        min: 5000,
                        max: 10000,
                    },
                    full(),
                    full(),
                    full(),
                    full(),
                    full(),
                    Parameter { min: 0, max: 0 },
                ],
                biome: "minecraft:hot".to_string(),
            },
        ]
    }

    #[test]
    fn nearest_biome_picks_the_closer_temperature_band() {
        let table = tiny_table();
        assert_eq!(
            nearest_biome(&table, &[-9000, 0, 0, 0, 0, 0, 0]),
            "minecraft:cold"
        );
        assert_eq!(
            nearest_biome(&table, &[9000, 0, 0, 0, 0, 0, 0]),
            "minecraft:hot"
        );
        // Exactly equidistant (target 0 is 5000 from both spans) must keep
        // the *earlier* table entry — vanilla's strict `<` tie-break.
        assert_eq!(nearest_biome(&table, &[0, 0, 0, 0, 0, 0, 0]), "minecraft:cold");
    }

    #[test]
    fn quantize_matches_java_float_truncation() {
        // (long)(0.8f * 10000.0f) == 8000, exact.
        assert_eq!(quantize_coord(0.8), 8000);
        // Negative truncates toward zero, not floor.
        assert_eq!(quantize_coord(-0.15), -1500);
    }

    fn synthetic_climate(y_sensitive_temperature: bool) -> ClimateSampler {
        let temperature = if y_sensitive_temperature {
            Density::YClampedGradient {
                from_y: -64.0,
                to_y: 320.0,
                from_value: -0.75,
                to_value: 0.75,
            }
        } else {
            Density::Const(0.123456789)
        };
        ClimateSampler {
            temperature,
            humidity: Density::Const(-0.23456789),
            continentalness: Density::Const(0.34567891),
            erosion: Density::Const(-0.45678912),
            depth: Density::YClampedGradient {
                from_y: -64.0,
                to_y: 320.0,
                from_value: 0.9,
                to_value: -0.9,
            },
            weirdness: Density::Const(0.56789123),
            xz_pure: [false; 6],
        }
        .with_xz_purity()
    }

    #[test]
    fn prepared_climate_reuses_proven_xz_channels_without_changing_target_bits() {
        let sampler = synthetic_climate(false);
        let prepared = sampler.prepare_xz(32, -16);
        for qz in 0..4 {
            for qx in 0..4 {
                let x = 32 + qx * 4;
                let z = -16 + qz * 4;
                for y in [-64, 0, 63, 319] {
                    assert_eq!(
                        sampler.target(x, y, z),
                        sampler.target_prepared(&prepared, x, y, z),
                        "prepared climate changed target at ({x}, {y}, {z})"
                    );
                }
            }
        }
    }

    #[test]
    fn prepared_climate_keeps_y_sensitive_channels_on_the_full_path() {
        let sampler = synthetic_climate(true);
        let prepared = sampler.prepare_xz(0, 0);
        for y in [-64, 0, 320] {
            assert_eq!(
                sampler.target(0, y, 0),
                sampler.target_prepared(&prepared, 0, y, 0)
            );
        }
        assert_ne!(
            sampler.target_prepared(&prepared, 0, -64, 0)[0],
            sampler.target_prepared(&prepared, 0, 320, 0)[0]
        );
        assert_ne!(
            sampler.target_prepared(&prepared, 0, -64, 0)[4],
            sampler.target_prepared(&prepared, 0, 320, 0)[4]
        );
    }

    #[test]
    fn prepared_climate_center_view_is_bit_identical() {
        let sampler = synthetic_climate(false);
        let prepared = sampler.prepare_xz_rect(-1, -1, 6, 6);
        let center = prepared.center_view();
        for qz in 0..4 {
            for qx in 0..4 {
                let x = qx * 4;
                let z = qz * 4;
                assert_eq!(
                    sampler.target(x, 80, z),
                    sampler.target_prepared_view(&center, x, 80, z),
                    "center climate view changed target at ({x}, {z})"
                );
            }
        }
    }

    #[test]
    #[should_panic(expected = "prepared climate lookup escaped its view")]
    fn prepared_climate_center_view_rejects_out_of_view_quart() {
        let sampler = synthetic_climate(false);
        let prepared = sampler.prepare_xz_rect(-1, -1, 6, 6);
        let center = prepared.center_view();
        sampler.target_prepared_view(&center, -4, 80, 0);
    }

    #[test]
    #[should_panic(expected = "prepared climate lookup escaped its view")]
    fn prepared_climate_view_rejects_non_quart_coordinates() {
        let sampler = synthetic_climate(false);
        let prepared = sampler.prepare_xz_rect(-1, -1, 6, 6);
        let center = prepared.center_view();
        sampler.target_prepared_view(&center, 0, 80, 1);
    }

    #[test]
    fn prepared_climate_matches_the_bundled_router_at_negative_and_positive_edges() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data");
        let resolver = SupportResolver { root: root.clone() };
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
        )
        .unwrap();
        let positions = [(-17, -19), (-1, 0), (0, 0), (1, 2), (31, 29)];
        let ys = [-64, -63, -1, 0, 31, 80, 200, 319];

        for seed in [0, 42, -8_823_894_646_i64] {
            let builder = Builder::new(seed, &resolver);
            let sampler = ClimateSampler::new(&settings, &builder);
            assert!(sampler.xz_pure.iter().any(|&pure| pure));
            for &(min_qx, min_qz) in &positions {
                let prepared = sampler.prepare_xz_rect(min_qx, min_qz, 6, 6);
                let center = prepared.center_view();
                for qz in min_qz..min_qz + 6 {
                    for qx in min_qx..min_qx + 6 {
                        let x = qx * 4;
                        let z = qz * 4;
                        for &y in &ys {
                            assert_eq!(
                                sampler.target(x, y, z),
                                sampler.target_prepared(&prepared, x, y, z),
                                "prepared router target changed at seed={seed}, x={x}, y={y}, z={z}"
                            );
                        }
                    }
                }
                for qz in min_qz + 1..min_qz + 5 {
                    for qx in min_qx + 1..min_qx + 5 {
                        let x = qx * 4;
                        let z = qz * 4;
                        assert_eq!(
                            sampler.target(x, 80, z),
                            sampler.target_prepared_view(&center, x, 80, z),
                            "center prepared router target changed at seed={seed}, x={x}, z={z}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn accepted_xz_purity_is_bit_invariant_across_y() {
        let sampler = synthetic_climate(false);
        for x in [-8, 0, 12] {
            for z in [-4, 0, 16] {
                for y in [-64, -1, 0, 127, 319] {
                    for other_y in [-63, 1, 200] {
                        for channel in 0..6 {
                            if sampler.xz_pure[channel] {
                                assert_eq!(
                                    sampler.channel(channel).compute(Context::new(x, y, z)).to_bits(),
                                    sampler
                                        .channel(channel)
                                        .compute(Context::new(x, other_y, z))
                                        .to_bits(),
                                    "accepted X/Z-pure channel {channel} changed with Y"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn parse_table_round_trips_row_order_and_fields() {
        let json: Value = serde_json::from_str(
            r#"[[-10000,10000,-10000,10000,-12000,-10500,-10000,10000,0,0,-10000,10000,0,"minecraft:mushroom_fields"],
                [-10000,-4500,-10000,10000,-10500,-4550,-10000,10000,10000,10000,-10000,10000,7,"minecraft:deep_frozen_ocean"]]"#,
        )
        .unwrap();
        let table = parse_table(&json);
        assert_eq!(table.len(), 2);
        assert_eq!(table[0].biome, "minecraft:mushroom_fields");
        assert_eq!(table[0].params[2], Parameter { min: -12000, max: -10500 }, "continentalness");
        assert_eq!(table[0].params[6], Parameter { min: 0, max: 0 }, "offset");
        assert_eq!(table[1].biome, "minecraft:deep_frozen_ocean");
        assert_eq!(table[1].params[6], Parameter { min: 7, max: 7 }, "offset");
    }

    #[test]
    fn consecutive_target_reuses_the_current_tied_incumbent() {
        let point = |value: i64, biome: &str| BiomeParameterPoint {
            params: [Parameter { min: value, max: value }; 7],
            biome: biome.to_owned(),
        };
        let table = BiomeTable::new(vec![
            point(0, "minecraft:first"),
            point(2, "minecraft:second"),
        ]);
        let mut cursor = table.search_cursor();
        let tie = [1; 7];
        assert_eq!(table.nearest_row_with_cursor(&[2; 7], &mut cursor), 1);
        assert_eq!(table.nearest_row_with_cursor(&tie, &mut cursor), 1);
        // The previous answer is the current tied incumbent, so a consecutive
        // repeat must return it without changing the stateful answer.
        assert_eq!(table.nearest_row_with_cursor(&tie, &mut cursor), 1);
    }

    /// `usable_overworld_table` used to filter `UNSUPPORTED_SURFACE_BIOMES`
    /// out because vanilla's own band lookup (`crate::surface::Rule::Bandlands`)
    /// was unported and would panic if a column ever resolved to one of the
    /// three. Now that the band lookup is ported, the exclusion is gone — this test
    /// used to assert the *old* filtering behaviour (named
    /// `usable_table_excludes_the_three_unported_badlands_variants`); it now
    /// asserts the opposite, as a real control rather than a renamed no-op:
    /// badlands entering the table must actually change what the nearest
    /// search returns, not merely survive being present in a `Vec`.
    #[test]
    fn usable_table_no_longer_excludes_the_three_formerly_unported_badlands_variants() {
        let json: Value = serde_json::from_str(
            r#"[[-10000,10000,-10000,10000,-10000,10000,-10000,10000,-10000,10000,-10000,10000,0,"minecraft:badlands"],
                [-10000,10000,-10000,10000,-10000,10000,-10000,10000,-10000,10000,-10000,10000,0,"minecraft:plains"]]"#,
        )
        .unwrap();
        let table = usable_overworld_table(parse_table(&json));
        assert_eq!(table.len(), 2, "usable_overworld_table must no longer drop any row");
        assert!(
            table.iter().any(|p| p.biome == "minecraft:badlands"),
            "badlands must survive usable_overworld_table now that getBand is ported"
        );
        // The control: with only two rows sharing identical climate spans but
        // different biome names, `nearest_biome` breaks the tie by table
        // order (first element wins ties in `fitness`'s strict `<`
        // comparison — see `nearest_biome`'s own loop). Since `parse_table`
        // preserves JSON row order and badlands is row 0 here, every target
        // must resolve to badlands — proving the search can actually select
        // it, not just that it's present in the `Vec`.
        for target in [
            [-10000, -10000, -10000, -10000, -10000, -10000, 0],
            [10000, 10000, 10000, 10000, 10000, 10000, 0],
            [0, 0, 0, 0, 0, 0, 0],
        ] {
            assert_eq!(nearest_biome(&table, &target), "minecraft:badlands");
        }
    }

    #[test]
    fn fingerprinted_tables_share_the_catalog_but_uncached_tables_do_not() {
        let points = tiny_table();
        let fingerprint = 0x9d7e_31a4_6b20_55c1;
        let cached_a = BiomeTable::new_cached(points.clone(), fingerprint);
        let cached_b = BiomeTable::new_cached(points.clone(), fingerprint);
        assert!(Arc::ptr_eq(&cached_a.data, &cached_b.data));
        assert_ne!(cached_a.id(), cached_b.id());
        let uncached_a = BiomeTable::new(points.clone());
        let uncached_b = BiomeTable::new(points);
        assert!(!Arc::ptr_eq(&uncached_a.data, &uncached_b.data));
        for target in [[-9000, 0, 0, 0, 0, 0, 0], [9000, 0, 0, 0, 0, 0, 0]] {
            assert_eq!(cached_a.nearest_row(&target), uncached_a.nearest_row(&target));
        }
    }

    #[test]
    fn cold_enough_to_snow_matches_known_biomes() {
        let mut temps = HashMap::new();
        temps.insert("minecraft:plains".to_string(), 0.8_f32);
        temps.insert("minecraft:snowy_taiga".to_string(), -0.5_f32);
        assert!(!cold_enough_to_snow(&temps, "minecraft:plains"));
        assert!(cold_enough_to_snow(&temps, "minecraft:snowy_taiga"));
        // Unknown biome: fail safe toward "cold" (matches the pre-existing
        // global default before this biome existed, `DEFAULT_BIOME_SNOWS`'s
        // sibling constant's own conservative choice is `false`, but an
        // *unknown* biome name is a data bug worth being loud about via snow
        // rather than silently matching the common case).
        assert!(cold_enough_to_snow(&temps, "minecraft:not_a_real_biome"));
    }
}
