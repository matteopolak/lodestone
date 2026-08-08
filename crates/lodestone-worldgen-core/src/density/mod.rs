//! A version-free interpreter for vanilla's data-driven density-function trees.
//!
//! Vanilla worldgen ships the noise router as data (963 JSON files in 26.2); this
//! module is the engine that evaluates that data. It parses a density-function
//! JSON node, resolves references and named noises through a [`Resolver`], seeds
//! each noise exactly as `RandomState` does (xoroshiro positional factory keyed
//! by the noise's registry id), and evaluates the resulting tree at a block
//! position.
//!
//! Point evaluation matches vanilla's `SinglePointContext` *value-wise*: no
//! marker wrapper ever changes *what* is computed, only how many times. **All
//! five marker kinds are now transparent here**, which is byte-for-byte what the
//! raw, uninstantiated `DensityFunctions.Marker.compute`
//! (`DensityFunctions.java:793-797`) does — `return this.wrapped.compute(context);`
//! and nothing else. `cache_2d` was the one exception until §12.132 measured its
//! hit rate at **0.12%**; see `## Caching`.
//!
//! ## Caching
//!
//! Vanilla only gives these markers real caching behaviour when a tree is
//! wrapped by `NoiseChunk::wrap` (`NoiseChunk.java:374-407`), which swaps each
//! marker for a `NoiseChunk`-private class carrying real cache state. This
//! evaluator has no `NoiseChunk` instance and is never wrapped that way — it
//! is vanilla's `SinglePointContext` path (`preliminary_surface_level`'s
//! `find_top_surface` scan, `spline`'s `coordinate` inputs, the aquifer's own
//! `preliminary_surface_level`) — so "what would vanilla's wrapped version
//! cache here" has to be answered per node kind, not assumed uniformly:
//!
//! * **`cache_2d`** (`NoiseChunk.Cache2D`, `NoiseChunk.java:531-569`) marks a
//!   subtree whose value is a pure function of `(x, z)`, and it is now
//!   **transparent** — the `Mutex`-backed single-slot last-`(x, z)` memo it
//!   carried from `d68e0a5` until §12.132 is gone. It is worth keeping *why*,
//!   because both the original decision and its reversal were measurements and
//!   the second one caught the first going stale:
//!
//!   The memo was added for `preliminarySurfaceLevel`'s own `cache2d(offset)` /
//!   `cache2d(factor)` wrapping (`NoiseRouterData.java:489-490`), which sits
//!   directly above `find_top_surface`'s per-`y` scan, and a criterion paired
//!   comparison measured **−4.4% (95% CI −6.0%..−2.7%, p < 0.05)** on `column()`'s
//!   median. That was true when written. §12.132 counted the outcome of every
//!   lookup over a 289-column burst and measured **24,843 hits against
//!   19,899,205 misses — a 0.12% hit rate**, 86 hits per column. So the scan the
//!   memo was for no longer re-enters it: U4 (§12.102) made `find_top_surface` a
//!   *leaf* whose result the `Scratch` slot layer memoises one level up, and that
//!   layer eliminated the repeat visits this one was catching.
//!
//!   Which makes the `flat_cache` paragraph below exactly right about `cache_2d`
//!   too: *"a last-value cache that (almost) never has a matching prior `(x, z)`
//!   pays a `Mutex` lock on every visit for (almost) no hits."* The asymmetry
//!   that paragraph claims — that `cache_2d` "sits directly over a scan that
//!   revisits one `(x, z)` dozens of times in a row" — is what stopped being
//!   true, and nothing about the code changed to announce it.
//!
//!   Under `Arc`-shared graph evaluation the cost was not just a lock: the
//!   compiled `final_density` carried **708** of these slots, reached ~68,900
//!   times per column, so 20 workers fought over 708 cache lines and IPC fell
//!   from **5.46 to 1.32**. Deleting the memo is value-invariant on real data by
//!   two independent arguments — vanilla's unwrapped `Marker.compute` does not
//!   memoise at all, and every `cache_2d` in 26.2's shipped data wraps an
//!   xz-only subtree — and the 45-column/5-seed dump is byte-identical across the
//!   change.
//! * **`flat_cache`** (`NoiseChunk.FlatCache`, `NoiseChunk.java:673-716`)
//!   marks the *same kind* of `(x, z)`-only boundary as `cache_2d` — vanilla's
//!   `overworld/continents.json` / `overworld/erosion.json` /
//!   `overworld/ridges.json` are each literally `flat_cache(shifted_noise(...,
//!   shift_y: 0.0, y_scale: 0.0))`, so the *value* reasoning above applies
//!   here too. It nonetheless stays **deliberately uncached, transparent —
//!   measured, not assumed.** A first attempt caching `flat_cache` the same
//!   way as `cache_2d` **regressed `column()`'s median by +11–13%** across
//!   every bench function (`worldgen/column_real_generator`,
//!   `column_timed_overhead`, both `linearity` scenes; all p < 0.05). Cause,
//!   confirmed by reading where `flat_cache` nodes actually sit in this
//!   crate's data: `continents`/`erosion`/`ridges` are reached almost
//!   entirely as `spline` `coordinate` inputs (`spline.rs`'s
//!   `coordinate.compute(ctx)`), and `spline` is one of
//!   [`NoiseChunkSampler`]'s designated "leaf" node kinds (`chunk.rs`) — i.e.
//!   every such call already arrives at a **distinct, already-deduplicated**
//!   `(x, z)` (one per unique interpolation corner, via
//!   [`NoiseChunkSampler`]'s own `slot_get` *before* raw `compute` is ever
//!   reached). A last-value cache that (almost) never has a matching prior
//!   `(x, z)` pays a `Mutex` lock on every visit for (almost) no hits. The
//!   asymmetry with `cache_2d` is call-site frequency and reuse shape, not
//!   node semantics: `cache_2d` in this router sits directly over a scan that
//!   revisits one `(x, z)` dozens of times in a row; `flat_cache` sits mostly
//!   over corner-leaf lookups that don't. Caching by node *kind* alone,
//!   ignoring where each kind is actually called from, would have shipped a
//!   net regression — the concrete instance of "measure, don't assume" this
//!   module's evidence standard exists for.
//! * **`interpolated`** (`NoiseChunk.NoiseInterpolator`, `NoiseChunk.java:735+`)
//!   marks a *different* boundary: vanilla only gives it real behaviour
//!   (trilinear interpolation between 4×8×4 cell corners) when driven by
//!   `NoiseChunk`'s own cell-filling loop state (`cellStartBlockY`,
//!   `inCellX/Y/Z`, `interpolationCounter`) — state this point evaluator has
//!   none of, and that [`NoiseChunkSampler`] (`chunk.rs`) already
//!   reimplements correctly and separately for the shape stage. Caching this
//!   node here (by whatever key) would be simulating the wrong machinery, not
//!   a slower version of the right one, so it stays transparent.
//! * **`cache_once`** and **`cache_all_in_cell`** (`NoiseChunk.CacheOnce`/
//!   `CacheAllInCell`, `NoiseChunk.java:571-644`) both explicitly check
//!   `context != NoiseChunk.this` and fall through to a plain, uncached
//!   `wrapped.compute(context)` whenever that holds — which is *always* true
//!   for a `SinglePointContext`, the only context this evaluator ever
//!   constructs. So even inside a real, wrapped `NoiseChunk`, these two are
//!   transparent for exactly the call shape used here; vanilla itself never
//!   caches them off the cell-filling loop. Concretely: `cache_once` wraps
//!   `sloped_cheese` (`NoiseRouterData.java:342`), a genuinely 3-D function —
//!   caching it by `(x, z)` alone the way `cache_2d` is cached above would
//!   silently return a stale value for a different `y` at the same `(x, z)`,
//!   which is exactly the "both slower and wrong" trap of treating every
//!   marker as one generic memo.
//! * **`blend_density`** (`NoiseChunk.BlendDensity`) only gets real behaviour
//!   when `!blender.isEmpty()` (`NoiseChunk.java:392-393`); with no blender
//!   this crate ever constructs, it is `wrapped` unchanged in vanilla too, so
//!   transparent is the correct — not merely unimplemented — behaviour.
//!
//! (The cell interpolation `interpolated` drives inside a real `NoiseChunk` is
//! a separate, later stage — [`NoiseChunkSampler`], not this module.)

use serde_json::Value;

use crate::math::{clamp, clamped_map};
use crate::noise::{BlendedNoise, NormalNoise};
use crate::rng::PositionalRandomFactory;

mod chunk;
mod spline;
pub use chunk::NoiseChunkSampler;
pub use spline::{Spline, SplinePoint};

/// Noise parameters loaded from a `worldgen/noise/*.json` file.
#[derive(Debug, Clone)]
pub struct NoiseParams {
    /// `firstOctave`.
    pub first_octave: i32,
    /// `amplitudes`.
    pub amplitudes: Vec<f64>,
}

/// Resolves references encountered while building a density-function tree.
pub trait Resolver {
    /// Loads the JSON body of another density function by id (e.g.
    /// `"minecraft:overworld/continents"`).
    fn density_function(&self, id: &str) -> Value;
    /// Loads noise parameters by id (e.g. `"minecraft:continentalness"`).
    fn noise(&self, id: &str) -> NoiseParams;

    /// The overworld multi-noise biome parameter table (issue #405), as the
    /// JSON array [`crate::biome::parse_table`] expects. Default: an empty
    /// array, meaning "no real biome variety supplied" —
    /// [`crate::overworld::OverworldGenerator`] falls back to its fixed
    /// constructor-supplied biome for every column, exactly as it did before
    /// this method existed. A resolver that wants real biome assignment (the
    /// bundled singleplayer generator, `lodestone-server::worldgen_data`)
    /// overrides this to return the embedded oracle dump. Kept as a
    /// *default* method rather than a required one so no existing `Resolver`
    /// implementor (fixture resolvers in this crate's own tests, benches,
    /// and `lodestone-world`'s pool-footprint test) needs to change to keep
    /// compiling.
    fn biome_parameters(&self) -> Value {
        Value::Array(Vec::new())
    }

    /// Per-biome `temperature` map (`{"minecraft:plains": 0.8, ...}`) as the
    /// JSON object [`crate::biome::parse_temperatures`] expects, used to
    /// derive each sampled column's `cold_enough_to_snow` answer. Default:
    /// an empty object — paired with an empty [`biome_parameters`](Self::biome_parameters),
    /// this is never consulted (the fixed-biome fallback path supplies its
    /// own `cold_enough_to_snow` directly).
    fn biome_temperatures(&self) -> Value {
        Value::Object(serde_json::Map::new())
    }

    /// The full `worldgen/biome/<name>.json` document for one biome (issue
    /// #295 composition): its `carvers` array and per-step `features` lists,
    /// consumed by [`crate::overworld::OverworldGenerator`] to select which
    /// carvers/ore features run for a given biome. Default: `Value::Null`,
    /// which every consumer of this method treats as "no carvers, no ore
    /// features for this biome" rather than panicking — the same
    /// no-data-supplied convention [`biome_parameters`](Self::biome_parameters)
    /// established, so a `Resolver` that only cares about shape/surface (most
    /// of this crate's own test fixtures) never needs to implement it.
    fn biome_document(&self, _id: &str) -> Value {
        Value::Null
    }

    /// `worldgen/configured_carver/<name>.json`. Default: `Value::Null` (see
    /// [`biome_document`](Self::biome_document)'s no-data convention).
    fn configured_carver(&self, _id: &str) -> Value {
        Value::Null
    }

    /// `worldgen/configured_feature/<name>.json`. Default: `Value::Null`.
    fn configured_feature(&self, _id: &str) -> Value {
        Value::Null
    }

    /// `worldgen/placed_feature/<name>.json`. Default: `Value::Null`.
    fn placed_feature(&self, _id: &str) -> Value {
        Value::Null
    }

    /// The five per-block-state predicates vanilla's `freeze_top_layer`
    /// (`TOP_LAYER_MODIFICATION`, issue #404's U2) needs, as the JSON document
    /// [`crate::feature::top_layer::SnowSupport::parse`] expects:
    ///
    /// ```json
    /// {
    ///   "blocks_motion":   { "default": ["minecraft:stone", ...], "states": {"...": false} },
    ///   "has_fluid_state": { "default": [...], "states": {...} },
    ///   "water_source":    { "default": [...], "states": {...} },
    ///   "face_full_up":    { "default": [...], "states": {...} },
    ///   "snowy_property":  { "default": [...], "states": {...} }
    /// }
    /// ```
    ///
    /// Each column is "the answer for every block's default state" plus an
    /// override for **every** state that disagrees with its own default — a
    /// complete, exact encoding, not a curated subset (see
    /// [`crate::feature::top_layer::StatePredicate`] for why the two-level shape
    /// exists and why the override list has to be exhaustive).
    ///
    /// Default: `Value::Null`, which parses to empty predicates and makes the
    /// whole step a no-op — the same "no data supplied" convention
    /// [`biome_parameters`](Self::biome_parameters) established. This is *not*
    /// datapack data: it is a census of the game's own compiled behaviour
    /// (collision geometry, fluid states), so a resolver that wants snow supplies
    /// it from `lodestone_data::snow_support` rather than from a JSON asset. See
    /// `lodestone_server::worldgen_data`'s implementation.
    fn block_freeze_facts(&self) -> Value {
        Value::Null
    }

    /// `tags/block/<name>.json` (the raw tag document, `{"values": [...]}`,
    /// with sub-tag references as `"#minecraft:..."` entries needing their
    /// own recursive lookup — see `crate::compose::resolve_block_tag`).
    /// Default: `Value::Null`, which resolves to an empty tag (no member
    /// blocks) rather than panicking.
    fn block_tag(&self, _id: &str) -> Value {
        Value::Null
    }

    /// Every `worldgen/structure_set/*.json` id this resolver can serve, e.g.
    /// `["minecraft:villages", "minecraft:shipwrecks", …]` (issue #514's S1).
    ///
    /// This is the *entry point* to the whole structure engine: vanilla's
    /// `ChunkGeneratorStructureState.createForNormal` iterates the structure-set
    /// registry, so a resolver that returns nothing here places no structures at
    /// all — the same "no data supplied" convention as
    /// [`biome_parameters`](Self::biome_parameters), and the reason
    /// `lodestone_worldgen::structure` is inert for every fixture resolver in
    /// this workspace without any of them changing.
    ///
    /// **Order is not significant and callers must not depend on it.**
    /// `lodestone_worldgen::structure::StructureRegistry` re-orders whatever it
    /// gets into vanilla's own bootstrap order (`StructureSets.bootstrap`), which
    /// is the order `createStructures` walks.
    fn structure_set_ids(&self) -> Vec<String> {
        Vec::new()
    }

    /// `worldgen/structure_set/<name>.json` — `{placement: {...}, structures: [...]}`.
    /// Default: `Value::Null` ("no such set").
    fn structure_set(&self, _id: &str) -> Value {
        Value::Null
    }

    /// `worldgen/structure/<name>.json` — the structure's `type`, `biomes`
    /// holder-set, `step`, `terrain_adaptation` and type-specific config.
    /// Default: `Value::Null`.
    fn structure(&self, _id: &str) -> Value {
        Value::Null
    }

    /// `tags/worldgen/biome/<name>.json`, the raw tag document
    /// (`{"values": [...]}`, with `"#minecraft:..."` entries needing their own
    /// recursive lookup, exactly like [`block_tag`](Self::block_tag)).
    ///
    /// Needed because **every** bundled structure spells its `biomes` field as a
    /// single tag reference (`"#minecraft:has_structure/shipwreck"`) rather than
    /// an inline list, so without this the biome predicate of every structure is
    /// empty and no start is ever valid. Default: `Value::Null` (empty tag).
    fn biome_tag(&self, _id: &str) -> Value {
        Value::Null
    }

    /// `structure/<path>.nbt` — one NBT **structure template**, as the raw file
    /// bytes (issue #514's S2). `minecraft:shipwreck/with_mast` means
    /// `assets/structure/shipwreck/with_mast.nbt`.
    ///
    /// Returned **exactly as shipped**, gzip wrapper included:
    /// `lodestone_worldgen::structure::template::StructureTemplate::parse` handles
    /// both gzipped and bare NBT, so a resolver never has to know which. Handing
    /// over the bytes rather than a parsed document is what keeps the NBT schema
    /// in one place instead of once per resolver.
    ///
    /// Default: `None` ("no such template"), the same no-data-supplied convention
    /// as [`biome_parameters`](Self::biome_parameters). A structure whose
    /// templates are missing is demoted to unsupported and named in
    /// `StructureRegistry::unsupported` — it never silently places nothing.
    fn structure_template(&self, _id: &str) -> Option<Vec<u8>> {
        None
    }

    /// `worldgen/template_pool/<name>.json` — one **jigsaw template pool**
    /// (`{fallback, elements: [{element, weight}]}`, issue #514's S4).
    ///
    /// The third entry point to the structure engine, after
    /// [`structure_set_ids`](Self::structure_set_ids) and
    /// [`structure_template`](Self::structure_template): a jigsaw structure names
    /// a `start_pool` and every jigsaw block inside a placed element names the
    /// next pool, so a resolver that supplies none of them makes every jigsaw
    /// structure (the five villages, `pillager_outpost`, `ancient_city`,
    /// `trail_ruins`, `trial_chambers`, the bastion) demote to `Unsupported` and
    /// appear in `StructureRegistry::unsupported` — placed, but with no blocks.
    ///
    /// Default: `Value::Null` ("no such pool"), the same no-data-supplied
    /// convention as [`biome_parameters`](Self::biome_parameters).
    fn template_pool(&self, _id: &str) -> Value {
        Value::Null
    }

    /// `worldgen/processor_list/<name>.json` — one named
    /// **structure-processor list** (`{"processors": [...]}`).
    ///
    /// A pool element spells its `processors` field either inline (an object) or
    /// as a reference to one of these 40 documents, so the reference form needs a
    /// lookup. Default: `Value::Null`, which resolves to an empty processor
    /// chain — an element then places its template unfiltered rather than not at
    /// all, so this one degrades quietly and is recorded on the ledger by name.
    fn processor_list(&self, _id: &str) -> Value {
        Value::Null
    }
}

/// Evaluation context: a single block position (`SinglePointContext`).
#[derive(Debug, Clone, Copy)]
pub struct Context {
    /// Block X.
    pub x: i32,
    /// Block Y.
    pub y: i32,
    /// Block Z.
    pub z: i32,
}

impl Context {
    /// Convenience constructor.
    #[must_use]
    pub fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }
}

/// A fully-instantiated, evaluatable density-function node.
#[derive(Debug, Clone)]
pub enum Density {
    /// A literal constant.
    Const(f64),
    /// `blend_alpha` — always `1.0` with the empty blender.
    BlendAlpha,
    /// `blend_offset` — always `0.0` with the empty blender.
    BlendOffset,
    /// `beardifier` — `0.0` outside structure placement.
    Beardifier,
    /// `y_clamped_gradient`.
    YClampedGradient {
        /// `from_y`.
        from_y: f64,
        /// `to_y`.
        to_y: f64,
        /// `from_value`.
        from_value: f64,
        /// `to_value`.
        to_value: f64,
    },
    /// `add`.
    Add(Box<Density>, Box<Density>),
    /// `mul` (with vanilla's `v1 == 0.0` short-circuit).
    Mul(Box<Density>, Box<Density>),
    /// `min`.
    Min(Box<Density>, Box<Density>),
    /// `max`.
    Max(Box<Density>, Box<Density>),
    /// `abs`.
    Abs(Box<Density>),
    /// `square`.
    Square(Box<Density>),
    /// `cube`.
    Cube(Box<Density>),
    /// `half_negative`.
    HalfNegative(Box<Density>),
    /// `quarter_negative`.
    QuarterNegative(Box<Density>),
    /// `squeeze`.
    Squeeze(Box<Density>),
    /// `invert` (`1.0 / x`).
    Invert(Box<Density>),
    /// `clamp`.
    Clamp {
        /// Wrapped input.
        input: Box<Density>,
        /// Lower bound.
        min: f64,
        /// Upper bound.
        max: f64,
    },
    /// `interpolated` — for point evaluation this is transparent (see the
    /// module's `## Caching` section for why), but for the block field it
    /// samples the wrapped fn at 4×8×4 cell corners and trilinearly
    /// interpolates (see [`chunk`]). `slot` indexes the sampler's per-node
    /// corner cache.
    Interpolated {
        /// Wrapped function.
        inner: Box<Density>,
        /// Cache slot for the block-field sampler.
        slot: usize,
    },
    /// `flat_cache` — for point evaluation this is transparent (module
    /// `## Caching`: same `(x, z)`-only value shape as `cache_2d`, but a real
    /// cache here measured as a net *regression*, so it deliberately stays
    /// uncached); for the block field it snaps XZ to the quart grid and
    /// forces `y = 0` (see [`chunk`], unaffected by this module's caching).
    FlatCache {
        /// Wrapped function.
        inner: Box<Density>,
        /// Cache slot for the block-field sampler.
        slot: usize,
    },
    /// `cache_2d` — **transparent in both evaluators** since §12.132 retired its
    /// 0.12%-hit-rate memo (module `## Caching` carries the measurement and the
    /// two independent value-invariance arguments). Kept as its own variant
    /// rather than folded into [`Marker`](Self::Marker) so
    /// [`kind_index`](Self::kind_index) — and therefore `engine::graph`'s
    /// `OpKind` discriminants — do not shift, and so
    /// `Program::cache_2d_under_leaves` still has something to count.
    Cache2D {
        /// Wrapped function.
        inner: Box<Density>,
    },
    /// A transparent marker (`cache_once`, `cache_all_in_cell`,
    /// `blend_density`): mathematically identical to its wrapped function in
    /// both point evaluation and the block field — see the module's
    /// `## Caching` section for why these three specifically stay
    /// transparent rather than joining `flat_cache`/`cache_2d`.
    Marker(Box<Density>),
    /// `noise`.
    Noise {
        /// The instantiated noise.
        noise: NormalNoise,
        /// `xz_scale`.
        xz_scale: f64,
        /// `y_scale`.
        y_scale: f64,
    },
    /// `shifted_noise`.
    ShiftedNoise {
        /// `shift_x`.
        shift_x: Box<Density>,
        /// `shift_y`.
        shift_y: Box<Density>,
        /// `shift_z`.
        shift_z: Box<Density>,
        /// `xz_scale`.
        xz_scale: f64,
        /// `y_scale`.
        y_scale: f64,
        /// The instantiated noise.
        noise: NormalNoise,
    },
    /// `shift_a` — offset noise sampled at `(x, 0, z)`.
    ShiftA(NormalNoise),
    /// `shift_b` — offset noise sampled at `(z, x, 0)`.
    ShiftB(NormalNoise),
    /// `shift` — offset noise sampled at `(x, y, z)`.
    Shift(NormalNoise),
    /// `range_choice`.
    RangeChoice {
        /// Selector input.
        input: Box<Density>,
        /// Inclusive lower bound.
        min_inclusive: f64,
        /// Exclusive upper bound.
        max_exclusive: f64,
        /// Branch when `input` is in `[min, max)`.
        when_in_range: Box<Density>,
        /// Branch otherwise.
        when_out_of_range: Box<Density>,
    },
    /// `interval_select`.
    IntervalSelect {
        /// Selector input.
        input: Box<Density>,
        /// Ascending thresholds (`functions.len() - 1` of them).
        thresholds: Vec<f64>,
        /// Branch functions.
        functions: Vec<Density>,
    },
    /// `spline`.
    Spline(Spline),
    /// `old_blended_noise`.
    Blended(BlendedNoise),
    /// `find_top_surface` — floors `upper_bound / cell_height` to a cell top,
    /// then scans down by `cell_height` returning the first `blockY` where
    /// `density(x, blockY, z) > 0.0`, else `lower_bound`.
    FindTopSurface {
        /// Density scanned per candidate Y.
        density: Box<Density>,
        /// Upper-bound density (evaluated at the query context).
        upper_bound: Box<Density>,
        /// Lower scan bound.
        lower_bound: i32,
        /// Cell step size.
        cell_height: i32,
    },
    /// `end_islands` — `DensityFunctions.EndIslandDensityFunction`, the End's
    /// island height field. A `SimpleFunction`: no children, no arguments, and
    /// **xz-only** (`compute(x, z)`; `y` is not read).
    ///
    /// Behind an [`Arc`] because construction burns 17,292 discarded `nextInt`s
    /// plus a 256-step shuffle, and the type appears **twice** in 26.2's data —
    /// `noise_settings/end.json`'s `erosion` channel (wrapped in `cache_2d`) and
    /// `density_function/end/sloped_cheese.json`. [`Builder`] therefore builds one
    /// and shares it; see [`Builder::build_object`]'s arm.
    ///
    /// Appended at the end of the enum on purpose — [`Self::kind_index`]'s values
    /// index recorded counter tables, so a variant inserted in the middle
    /// renumbers every later kind.
    EndIslands(std::sync::Arc<crate::noise::EndIslandNoise>),
}

impl Density {
    /// Number of [`Density`] variants — the width of the per-kind counter
    /// arrays in [`crate::counters`].
    ///
    /// Adding a variant without extending [`Self::kind_index`]'s `match` is a
    /// compile error (it is exhaustive), and `tests::kind_names_are_distinct`
    /// plus `tests::kind_index_matches_names_for_constructible_variants` check
    /// the rest. **The residual gap, stated rather than glossed:** an exhaustive
    /// match would still compile if two variants shared one index, and the
    /// index test can only cover the variants cheap to construct (24 of 32 — the
    /// eight needing a `NormalNoise`/`BlendedNoise`/`Spline`/`EndIslandNoise`
    /// payload are checked
    /// by reading, not by assertion). Distinctness of the *names* is asserted for
    /// all 32, which catches a copy-paste in the table itself.
    pub const KIND_COUNT: usize = 32;

    /// Human-readable variant names, indexed by [`Self::kind_index`].
    pub const KIND_NAMES: [&'static str; Self::KIND_COUNT] = [
        "const",
        "blend_alpha",
        "blend_offset",
        "beardifier",
        "y_clamped_gradient",
        "add",
        "mul",
        "min",
        "max",
        "abs",
        "square",
        "cube",
        "half_negative",
        "quarter_negative",
        "squeeze",
        "invert",
        "clamp",
        "interpolated",
        "flat_cache",
        "cache_2d",
        "marker",
        "noise",
        "shifted_noise",
        "shift_a",
        "shift_b",
        "shift",
        "range_choice",
        "interval_select",
        "spline",
        "blended",
        "find_top_surface",
        "end_islands",
    ];

    /// This node's variant as a dense index into `0..KIND_COUNT`.
    ///
    /// Exists so [`crate::counters`] can count evaluations *by kind* with a
    /// single array index rather than a match at every hook, which is what makes
    /// "which component kind dominates the tree walk" answerable — diagnostic D1
    /// in `docs/plans/worldgen-rewrite.md`. `std::mem::discriminant` cannot be
    /// used: it is deliberately opaque and yields no index.
    ///
    /// The order is the declaration order of the enum, and
    /// [`Self::KIND_NAMES`] must stay in that same order. Insert new variants at
    /// the **end** of both, not in the middle: a recorded counter table from an
    /// earlier run is indexed by these numbers.
    #[must_use]
    pub fn kind_index(&self) -> usize {
        match self {
            Density::Const { .. } => 0,
            Density::BlendAlpha { .. } => 1,
            Density::BlendOffset { .. } => 2,
            Density::Beardifier { .. } => 3,
            Density::YClampedGradient { .. } => 4,
            Density::Add { .. } => 5,
            Density::Mul { .. } => 6,
            Density::Min { .. } => 7,
            Density::Max { .. } => 8,
            Density::Abs { .. } => 9,
            Density::Square { .. } => 10,
            Density::Cube { .. } => 11,
            Density::HalfNegative { .. } => 12,
            Density::QuarterNegative { .. } => 13,
            Density::Squeeze { .. } => 14,
            Density::Invert { .. } => 15,
            Density::Clamp { .. } => 16,
            Density::Interpolated { .. } => 17,
            Density::FlatCache { .. } => 18,
            Density::Cache2D { .. } => 19,
            Density::Marker { .. } => 20,
            Density::Noise { .. } => 21,
            Density::ShiftedNoise { .. } => 22,
            Density::ShiftA { .. } => 23,
            Density::ShiftB { .. } => 24,
            Density::Shift { .. } => 25,
            Density::RangeChoice { .. } => 26,
            Density::IntervalSelect { .. } => 27,
            Density::Spline { .. } => 28,
            Density::Blended { .. } => 29,
            Density::FindTopSurface { .. } => 30,
            Density::EndIslands { .. } => 31,
        }
    }

    /// Appends a **complete, bit-exact** description of this subtree to `out`.
    ///
    /// Two `Density` values produce equal signatures iff they are bit-identical
    /// in every field, transitively — see
    /// [`crate::noise::ImprovedNoise::write_signature`] for the contract and the
    /// float/`NaN` traps it exists to avoid. `engine::graph`'s node-sharing pass
    /// uses this to decide that two separately-built copies of one subtree are
    /// the *same* function and can share one compiled node, so an
    /// under-specified signature here is a wrong-terrain bug, not a missed
    /// optimisation.
    ///
    /// **`slot` is deliberately excluded** for `interpolated`/`flat_cache`. A
    /// slot is an index into the *evaluator's* per-chunk memo, not part of the
    /// function the node denotes: two `flat_cache` nodes over the same inner
    /// compute the same value at every position regardless of which slot they
    /// were assigned, so collapsing them onto one slot is value-invariant and is
    /// exactly the duplication §12.132 measured. Every other field of every
    /// variant is included.
    ///
    /// **How to extend this.** A new `Density` variant must add an arm here, and
    /// the discriminant word must be [`Self::kind_index`] so no two variants can
    /// collide. Lengths precede their contents (see
    /// [`crate::noise::PerlinNoise::write_signature`] for why). If a new leaf
    /// kind is *not* a pure function of position — anything that would advance an
    /// RNG, read wall-clock state, or otherwise depend on evaluation order — it
    /// must **not** be given a signature arm that lets it dedupe; make it
    /// `unreachable_signature` (push a per-instance unique word) or exclude its
    /// `OpKind` from `engine::graph`'s interner. Nothing in 26.2's density data
    /// is in that class today.
    pub fn write_signature(&self, out: &mut Vec<u64>) {
        out.push(self.kind_index() as u64);
        match self {
            Density::Const(v) => out.push(v.to_bits()),
            Density::BlendAlpha | Density::BlendOffset | Density::Beardifier => {}
            Density::YClampedGradient {
                from_y,
                to_y,
                from_value,
                to_value,
            } => {
                for v in [from_y, to_y, from_value, to_value] {
                    out.push(v.to_bits());
                }
            }
            Density::Add(a, b) | Density::Mul(a, b) | Density::Min(a, b) | Density::Max(a, b) => {
                a.write_signature(out);
                b.write_signature(out);
            }
            Density::Abs(a)
            | Density::Square(a)
            | Density::Cube(a)
            | Density::HalfNegative(a)
            | Density::QuarterNegative(a)
            | Density::Squeeze(a)
            | Density::Invert(a)
            | Density::Marker(a) => a.write_signature(out),
            Density::Clamp { input, min, max } => {
                input.write_signature(out);
                out.push(min.to_bits());
                out.push(max.to_bits());
            }
            // `slot` excluded — see the doc comment.
            Density::Interpolated { inner, slot: _ }
            | Density::FlatCache { inner, slot: _ }
            | Density::Cache2D { inner } => inner.write_signature(out),
            Density::Noise {
                noise,
                xz_scale,
                y_scale,
            } => {
                noise.write_signature(out);
                out.push(xz_scale.to_bits());
                out.push(y_scale.to_bits());
            }
            Density::ShiftedNoise {
                shift_x,
                shift_y,
                shift_z,
                xz_scale,
                y_scale,
                noise,
            } => {
                shift_x.write_signature(out);
                shift_y.write_signature(out);
                shift_z.write_signature(out);
                out.push(xz_scale.to_bits());
                out.push(y_scale.to_bits());
                noise.write_signature(out);
            }
            Density::ShiftA(n) | Density::ShiftB(n) | Density::Shift(n) => n.write_signature(out),
            Density::RangeChoice {
                input,
                min_inclusive,
                max_exclusive,
                when_in_range,
                when_out_of_range,
            } => {
                input.write_signature(out);
                out.push(min_inclusive.to_bits());
                out.push(max_exclusive.to_bits());
                when_in_range.write_signature(out);
                when_out_of_range.write_signature(out);
            }
            Density::IntervalSelect {
                input,
                thresholds,
                functions,
            } => {
                input.write_signature(out);
                out.push(thresholds.len() as u64);
                out.extend(thresholds.iter().map(|t| t.to_bits()));
                out.push(functions.len() as u64);
                for f in functions {
                    f.write_signature(out);
                }
            }
            Density::EndIslands(n) => n.write_signature(out),
            Density::Spline(s) => s.write_signature(out),
            Density::Blended(b) => b.write_signature(out),
            Density::FindTopSurface {
                density,
                upper_bound,
                lower_bound,
                cell_height,
            } => {
                density.write_signature(out);
                upper_bound.write_signature(out);
                out.push(*lower_bound as u32 as u64);
                out.push(*cell_height as u32 as u64);
            }
        }
    }

    /// Evaluates the node at `ctx`.
    #[must_use]
    pub fn compute(&self, ctx: Context) -> f64 {
        crate::counters::bump_density_point_compute(self.kind_index());
        match self {
            Density::Const(v) => *v,
            Density::BlendAlpha => 1.0,
            Density::BlendOffset | Density::Beardifier => 0.0,
            Density::YClampedGradient {
                from_y,
                to_y,
                from_value,
                to_value,
            } => clamped_map(f64::from(ctx.y), *from_y, *to_y, *from_value, *to_value),
            Density::Add(a, b) => a.compute(ctx) + b.compute(ctx),
            Density::Mul(a, b) => {
                let v1 = a.compute(ctx);
                if v1 == 0.0 { 0.0 } else { v1 * b.compute(ctx) }
            }
            Density::Min(a, b) => a.compute(ctx).min(b.compute(ctx)),
            Density::Max(a, b) => a.compute(ctx).max(b.compute(ctx)),
            Density::Abs(a) => a.compute(ctx).abs(),
            Density::Square(a) => {
                let v = a.compute(ctx);
                v * v
            }
            Density::Cube(a) => {
                let v = a.compute(ctx);
                v * v * v
            }
            Density::HalfNegative(a) => {
                let v = a.compute(ctx);
                if v > 0.0 { v } else { v * 0.5 }
            }
            Density::QuarterNegative(a) => {
                let v = a.compute(ctx);
                if v > 0.0 { v } else { v * 0.25 }
            }
            Density::Squeeze(a) => {
                let c = clamp(a.compute(ctx), -1.0, 1.0);
                c / 2.0 - c * c * c / 24.0
            }
            Density::Invert(a) => 1.0 / a.compute(ctx),
            Density::Clamp { input, min, max } => clamp(input.compute(ctx), *min, *max),
            Density::Interpolated { inner, .. }
            | Density::Marker(inner)
            | Density::FlatCache { inner, .. }
            | Density::Cache2D { inner } => inner.compute(ctx),
            Density::Noise {
                noise,
                xz_scale,
                y_scale,
            } => noise.get_value(
                f64::from(ctx.x) * xz_scale,
                f64::from(ctx.y) * y_scale,
                f64::from(ctx.z) * xz_scale,
            ),
            Density::ShiftedNoise {
                shift_x,
                shift_y,
                shift_z,
                xz_scale,
                y_scale,
                noise,
            } => {
                let x = f64::from(ctx.x) * xz_scale + shift_x.compute(ctx);
                let y = f64::from(ctx.y) * y_scale + shift_y.compute(ctx);
                let z = f64::from(ctx.z) * xz_scale + shift_z.compute(ctx);
                noise.get_value(x, y, z)
            }
            Density::ShiftA(noise) => shift_compute(noise, f64::from(ctx.x), 0.0, f64::from(ctx.z)),
            Density::ShiftB(noise) => shift_compute(noise, f64::from(ctx.z), f64::from(ctx.x), 0.0),
            Density::Shift(noise) => {
                shift_compute(noise, f64::from(ctx.x), f64::from(ctx.y), f64::from(ctx.z))
            }
            Density::RangeChoice {
                input,
                min_inclusive,
                max_exclusive,
                when_in_range,
                when_out_of_range,
            } => {
                let v = input.compute(ctx);
                if v >= *min_inclusive && v < *max_exclusive {
                    when_in_range.compute(ctx)
                } else {
                    when_out_of_range.compute(ctx)
                }
            }
            Density::IntervalSelect {
                input,
                thresholds,
                functions,
            } => {
                let v = input.compute(ctx);
                for (i, t) in thresholds.iter().enumerate() {
                    if v < *t {
                        return functions[i].compute(ctx);
                    }
                }
                functions[functions.len() - 1].compute(ctx)
            }
            Density::Spline(s) => f64::from(s.compute(ctx)),
            Density::Blended(b) => b.compute(ctx.x, ctx.y, ctx.z),
            // xz-only: `y` is deliberately not passed. `EndIslandDensityFunction`
            // takes `(blockX, blockZ)` and the End's `erosion` channel wraps it in
            // `cache_2d` for exactly that reason.
            Density::EndIslands(n) => n.compute(ctx.x, ctx.z),
            Density::FindTopSurface {
                density,
                upper_bound,
                lower_bound,
                cell_height,
            } => {
                let lower = *lower_bound;
                let step = *cell_height;
                let top_y = (upper_bound.compute(ctx) / f64::from(step)).floor() as i32 * step;
                if top_y <= lower {
                    return f64::from(lower);
                }
                let mut block_y = top_y;
                while block_y >= lower {
                    if density.compute(Context::new(ctx.x, block_y, ctx.z)) > 0.0 {
                        return f64::from(block_y);
                    }
                    block_y -= step;
                }
                f64::from(lower)
            }
        }
    }
}

fn shift_compute(noise: &NormalNoise, x: f64, y: f64, z: f64) -> f64 {
    noise.get_value(x * 0.25, y * 0.25, z * 0.25) * 4.0
}

/// Builds a density-function tree from JSON, seeding all noises from `seed`
/// exactly as vanilla's `RandomState` does.
///
/// The RNG family is **data**, not a constant: `legacy_random_source` in the
/// dimension's `noise_settings` picks it, and the Nether and the End both set
/// it. [`Builder::new`] keeps the xoroshiro default the Overworld uses;
/// [`Builder::with_algorithm`] is the dimension-aware entry point. See
/// [`crate::rng::Algorithm`].
#[allow(missing_debug_implementations)]
pub struct Builder<'a> {
    master: crate::rng::AnyPositionalFactory,
    /// The raw world seed. Kept because two of `RandomState`'s wirings are
    /// defined on it directly rather than on a positional fork: the two Nether
    /// biome noises (`seed + 0` / `seed + 1`) and `BlendedNoise` under legacy
    /// init (`seed + 0`).
    seed: i64,
    algorithm: crate::rng::Algorithm,
    resolver: &'a dyn Resolver,
    slots: std::cell::Cell<usize>,
    /// The one `end_islands` instance this builder hands to every occurrence.
    ///
    /// Built lazily and **exactly once**, which is a correctness requirement and
    /// not only a cost one: `EndIslandNoise::new` burns 17,292 discarded
    /// `nextInt`s plus a 256-step shuffle, and vanilla's
    /// `EndIslandDensityFunction` is *one object* substituted into both of the
    /// type's occurrences in 26.2's data (`noise_settings/end.json`'s `erosion`
    /// and `density_function/end/sloped_cheese.json`). Constructing it twice is
    /// value-identical here — it reads a fresh `LegacyRandomSource(seed)` and
    /// advances no shared stream, the same property `engine::graph`'s node-sharing
    /// pass relies on — but it is ~35,000 wasted draws per builder, and sharing
    /// the `Arc` also lets the compiler's leaf table hold one copy.
    end_islands: std::cell::OnceCell<std::sync::Arc<crate::noise::EndIslandNoise>>,
}

/// `Noises.TEMPERATURE_NETHER` — one of the two ids `RandomState` special-cases.
const TEMPERATURE_NETHER: &str = "minecraft:nether/temperature";
/// `Noises.VEGETATION_NETHER`.
const VEGETATION_NETHER: &str = "minecraft:nether/vegetation";

impl<'a> Builder<'a> {
    /// Creates a builder for `seed` using `resolver` for references, with the
    /// xoroshiro family — i.e. every dimension whose settings leave
    /// `legacy_random_source` false, which is the Overworld and its variants.
    pub fn new(seed: i64, resolver: &'a dyn Resolver) -> Self {
        Self::with_algorithm(seed, crate::rng::Algorithm::Xoroshiro, resolver)
    }

    /// Creates a builder for `seed` on an explicit RNG family — the
    /// `settings.getRandomSource().newInstance(seed).forkPositional()` of
    /// `RandomState.java:36`.
    ///
    /// Use [`crate::rng::Algorithm::from_settings`] to read the flag rather than
    /// deciding per dimension at the call site.
    pub fn with_algorithm(
        seed: i64,
        algorithm: crate::rng::Algorithm,
        resolver: &'a dyn Resolver,
    ) -> Self {
        Self {
            master: algorithm.root_positional(seed),
            seed,
            algorithm,
            resolver,
            slots: std::cell::Cell::new(0),
            end_islands: std::cell::OnceCell::new(),
        }
    }

    /// Number of `interpolated`/`flat_cache` nodes assigned a cache slot so far
    /// (i.e. after [`build`](Self::build)). The block-field sampler allocates one
    /// cache per slot.
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.slots.get()
    }

    fn next_slot(&self) -> usize {
        let n = self.slots.get();
        self.slots.set(n + 1);
        n
    }

    fn instantiate_noise(&self, id: &str) -> NormalNoise {
        let params = self.resolver.noise(id);
        // `RandomState.NoiseWiringHelper.visitNoise` (`RandomState.java:53-65`)
        // branches on the noise *id*, before `getOrCreateNoise` is ever reached,
        // and does so regardless of `legacy_random_source` — so this fork is not
        // conditional on `self.algorithm`. `newLegacyInstance(n)` is
        // `new LegacyRandomSource(seed + n)` on the raw world seed (`:50-52`).
        let offset = match id {
            TEMPERATURE_NETHER => Some(0),
            VEGETATION_NETHER => Some(1),
            _ => None,
        };
        if let Some(offset) = offset {
            let mut src = crate::rng::LegacyRandomSource::new(self.seed.wrapping_add(offset));
            return NormalNoise::create_legacy_nether_biome(
                &mut src,
                params.first_octave,
                &params.amplitudes,
            );
        }
        let mut src = self.master.from_hash_of(id);
        NormalNoise::create(&mut src, params.first_octave, &params.amplitudes)
    }

    /// Instantiates a `NormalNoise` by id, seeded exactly as vanilla's
    /// `RandomState.getOrCreateNoise` (`master.fromHashOf(id)`). Used by the
    /// surface system for its `surface`/`surface_secondary` and
    /// `noise_threshold` noises.
    #[must_use]
    pub fn noise(&self, id: &str) -> NormalNoise {
        self.instantiate_noise(id)
    }

    /// The root positional factory (`RandomState.random`), forked from the
    /// dimension seed. The surface system reuses this for `getSurfaceDepth`'s
    /// per-column `at(x, 0, z)` draw and for `getOrCreateRandomFactory(name)`
    /// (`master.fromHashOf(name).forkPositional()`) used by `vertical_gradient`.
    #[must_use]
    pub fn positional_factory(&self) -> crate::rng::AnyPositionalFactory {
        self.master
    }

    /// The RNG family this builder was created with — the `useLegacyInit` of
    /// `RandomState.java:41`. Exposed so a dimension pipeline can assert it read
    /// its own settings rather than inheriting the Overworld default.
    #[must_use]
    pub fn algorithm(&self) -> crate::rng::Algorithm {
        self.algorithm
    }

    fn instantiate_blended(&self, node: &Value) -> BlendedNoise {
        // `RandomState.java:70-73`: `useLegacyInit ? newLegacyInstance(0L)
        // : random.fromHashOf("terrain")`. Both `old_blended_noise` dimensions
        // (Nether, End) take the first arm, on the raw world seed.
        let mut src = if self.algorithm.is_legacy() {
            crate::rng::AnyRandomSource::Legacy(crate::rng::LegacyRandomSource::new(self.seed))
        } else {
            self.master.from_hash_of("minecraft:terrain")
        };
        BlendedNoise::new(
            &mut src,
            f(node, "xz_scale"),
            f(node, "y_scale"),
            f(node, "xz_factor"),
            f(node, "y_factor"),
            f(node, "smear_scale_multiplier"),
        )
    }

    /// Parses and instantiates a density-function value (number, ref string, or
    /// typed object).
    pub fn build(&self, node: &Value) -> Density {
        match node {
            Value::Number(n) => Density::Const(n.as_f64().unwrap()),
            Value::String(id) => {
                let referenced = self.resolver.density_function(id);
                self.build(&referenced)
            }
            Value::Object(_) => self.build_object(node),
            other => panic!("unexpected density-function json: {other:?}"),
        }
    }

    fn child(&self, node: &Value, key: &str) -> Box<Density> {
        Box::new(self.build(&node[key]))
    }

    fn build_object(&self, node: &Value) -> Density {
        let ty = node
            .get("type")
            .and_then(Value::as_str)
            .expect("density function object missing type")
            .strip_prefix("minecraft:")
            .expect("expected minecraft-namespaced type");
        match ty {
            "constant" => Density::Const(f(node, "argument")),
            "blend_alpha" => Density::BlendAlpha,
            "blend_offset" => Density::BlendOffset,
            "beardifier" => Density::Beardifier,
            "y_clamped_gradient" => Density::YClampedGradient {
                from_y: f(node, "from_y"),
                to_y: f(node, "to_y"),
                from_value: f(node, "from_value"),
                to_value: f(node, "to_value"),
            },
            "add" => Density::Add(self.child(node, "argument1"), self.child(node, "argument2")),
            "mul" => Density::Mul(self.child(node, "argument1"), self.child(node, "argument2")),
            "min" => Density::Min(self.child(node, "argument1"), self.child(node, "argument2")),
            "max" => Density::Max(self.child(node, "argument1"), self.child(node, "argument2")),
            "abs" => Density::Abs(self.child(node, "argument")),
            "square" => Density::Square(self.child(node, "argument")),
            "cube" => Density::Cube(self.child(node, "argument")),
            "half_negative" => Density::HalfNegative(self.child(node, "argument")),
            "quarter_negative" => Density::QuarterNegative(self.child(node, "argument")),
            "squeeze" => Density::Squeeze(self.child(node, "argument")),
            "invert" => Density::Invert(self.child(node, "argument")),
            "clamp" => Density::Clamp {
                input: self.child(node, "input"),
                min: f(node, "min"),
                max: f(node, "max"),
            },
            "interpolated" => Density::Interpolated {
                inner: self.child(node, "argument"),
                slot: self.next_slot(),
            },
            "flat_cache" => Density::FlatCache {
                inner: self.child(node, "argument"),
                slot: self.next_slot(),
            },
            "cache_2d" => Density::Cache2D {
                inner: self.child(node, "argument"),
            },
            "cache_once" | "cache_all_in_cell" | "blend_density" => {
                Density::Marker(self.child(node, "argument"))
            }
            "noise" => Density::Noise {
                noise: self.instantiate_noise(node["noise"].as_str().unwrap()),
                xz_scale: f(node, "xz_scale"),
                y_scale: f(node, "y_scale"),
            },
            "shifted_noise" => Density::ShiftedNoise {
                shift_x: self.child(node, "shift_x"),
                shift_y: self.child(node, "shift_y"),
                shift_z: self.child(node, "shift_z"),
                xz_scale: f(node, "xz_scale"),
                y_scale: f(node, "y_scale"),
                noise: self.instantiate_noise(node["noise"].as_str().unwrap()),
            },
            "shift_a" => {
                Density::ShiftA(self.instantiate_noise(node["argument"].as_str().unwrap()))
            }
            "shift_b" => {
                Density::ShiftB(self.instantiate_noise(node["argument"].as_str().unwrap()))
            }
            "shift" => Density::Shift(self.instantiate_noise(node["argument"].as_str().unwrap())),
            "range_choice" => Density::RangeChoice {
                input: self.child(node, "input"),
                min_inclusive: f(node, "min_inclusive"),
                max_exclusive: f(node, "max_exclusive"),
                when_in_range: self.child(node, "when_in_range"),
                when_out_of_range: self.child(node, "when_out_of_range"),
            },
            "interval_select" => {
                let thresholds = node["thresholds"]
                    .as_array()
                    .map(|a| a.iter().map(|v| v.as_f64().unwrap()).collect())
                    .unwrap_or_default();
                let functions = node["functions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| self.build(v))
                    .collect();
                Density::IntervalSelect {
                    input: self.child(node, "input"),
                    thresholds,
                    functions,
                }
            }
            // `MapCodec.unit(new EndIslandDensityFunction(0L))` — the document
            // carries no arguments at all and always deserialises with seed 0;
            // `RandomState.java:74` substitutes the raw world seed afterwards,
            // which is `self.seed` and *not* a positional fork.
            "end_islands" => Density::EndIslands(std::sync::Arc::clone(
                self.end_islands.get_or_init(|| {
                    std::sync::Arc::new(crate::noise::EndIslandNoise::new(self.seed))
                }),
            )),
            "spline" => Density::Spline(self.build_spline(&node["spline"])),
            "old_blended_noise" => Density::Blended(self.instantiate_blended(node)),
            "find_top_surface" => Density::FindTopSurface {
                density: self.child(node, "density"),
                upper_bound: self.child(node, "upper_bound"),
                lower_bound: node["lower_bound"].as_i64().unwrap() as i32,
                cell_height: node["cell_height"].as_i64().unwrap() as i32,
            },
            other => panic!("unhandled density-function type: minecraft:{other}"),
        }
    }

    fn build_spline(&self, node: &Value) -> Spline {
        if let Some(n) = node.as_f64() {
            return Spline::Constant(n as f32);
        }
        let coordinate = Box::new(self.build(&node["coordinate"]));
        let points = node["points"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| SplinePoint {
                location: f(p, "location") as f32,
                derivative: f(p, "derivative") as f32,
                value: Box::new(self.build_spline(&p["value"])),
            })
            .collect();
        Spline::Multipoint { coordinate, points }
    }
}

fn f(node: &Value, key: &str) -> f64 {
    node[key]
        .as_f64()
        .unwrap_or_else(|| panic!("missing/non-numeric field {key}"))
}

#[cfg(test)]
mod kind_index_tests {
    use super::Density;

    /// The name table must be exactly as wide as the index space and carry no
    /// duplicate — a copy-paste in [`Density::KIND_NAMES`] would silently make
    /// two counter buckets report under one label, which is the failure mode
    /// that makes a per-kind counter table lie without ever looking wrong.
    #[test]
    fn kind_names_are_distinct_and_complete() {
        assert_eq!(Density::KIND_NAMES.len(), Density::KIND_COUNT);
        let mut sorted = Density::KIND_NAMES;
        sorted.sort_unstable();
        for pair in sorted.windows(2) {
            assert_ne!(
                pair[0], pair[1],
                "duplicate kind name {:?} in Density::KIND_NAMES — two counter \
                 buckets would report under one label",
                pair[0]
            );
        }
    }

    /// Every index [`Density::kind_index`] returns must be in range, and the
    /// variants cheap enough to construct must map to *distinct* indices whose
    /// [`Density::KIND_NAMES`] entry is the expected string.
    ///
    /// The exhaustive `match` in `kind_index` guarantees total coverage but
    /// **not** injectivity — it would compile happily with two variants sharing
    /// an index. This is the control for that. It covers 24 of the 32 variants;
    /// the eight needing a `NormalNoise`/`BlendedNoise`/`Spline`/`EndIslandNoise`
    /// payload
    /// (`noise`, `shifted_noise`, `shift_a`, `shift_b`, `shift`, `spline`,
    /// `blended`) are not constructible here without a resolver and are checked
    /// by reading the match. Stated as a known gap rather than implied by
    /// silence.
    #[test]
    fn kind_index_is_injective_over_constructible_variants() {
        let b = || Box::new(Density::Const(0.0));
        let cases: Vec<(Density, &str)> = vec![
            (Density::Const(0.0), "const"),
            (Density::BlendAlpha, "blend_alpha"),
            (Density::BlendOffset, "blend_offset"),
            (Density::Beardifier, "beardifier"),
            (
                Density::YClampedGradient {
                    from_y: 0.0,
                    to_y: 1.0,
                    from_value: 0.0,
                    to_value: 1.0,
                },
                "y_clamped_gradient",
            ),
            (Density::Add(b(), b()), "add"),
            (Density::Mul(b(), b()), "mul"),
            (Density::Min(b(), b()), "min"),
            (Density::Max(b(), b()), "max"),
            (Density::Abs(b()), "abs"),
            (Density::Square(b()), "square"),
            (Density::Cube(b()), "cube"),
            (Density::HalfNegative(b()), "half_negative"),
            (Density::QuarterNegative(b()), "quarter_negative"),
            (Density::Squeeze(b()), "squeeze"),
            (Density::Invert(b()), "invert"),
            (
                Density::Clamp {
                    input: b(),
                    min: 0.0,
                    max: 1.0,
                },
                "clamp",
            ),
            (
                Density::Interpolated { inner: b(), slot: 0 },
                "interpolated",
            ),
            (Density::FlatCache { inner: b(), slot: 0 }, "flat_cache"),
            (
                Density::Cache2D { inner: b() },
                "cache_2d",
            ),
            (Density::Marker(b()), "marker"),
            (
                Density::RangeChoice {
                    input: b(),
                    min_inclusive: 0.0,
                    max_exclusive: 1.0,
                    when_in_range: b(),
                    when_out_of_range: b(),
                },
                "range_choice",
            ),
            (
                Density::FindTopSurface {
                    density: b(),
                    upper_bound: b(),
                    lower_bound: 0,
                    cell_height: 8,
                },
                "find_top_surface",
            ),
        ];

        let mut seen: Vec<usize> = Vec::new();
        for (node, expected_name) in &cases {
            let i = node.kind_index();
            assert!(
                i < Density::KIND_COUNT,
                "kind_index returned {i} for {expected_name}, out of range"
            );
            assert_eq!(
                Density::KIND_NAMES[i], *expected_name,
                "kind_index({expected_name}) = {i} names {:?} — the match and \
                 KIND_NAMES have drifted out of order",
                Density::KIND_NAMES[i]
            );
            assert!(
                !seen.contains(&i),
                "index {i} ({expected_name}) is already claimed by another \
                 variant — kind_index is not injective, so two counter buckets \
                 would be summed into one"
            );
            seen.push(i);
        }
        // Control on the control: if this list ever shrinks to nothing (a
        // refactor deleting the cases while keeping the test), the assertions
        // above are all vacuously satisfied and this test passes while checking
        // nothing.
        assert_eq!(
            seen.len(),
            23,
            "the constructible-variant list changed size; update this count \
             deliberately rather than letting the test measure fewer variants \
             than it claims"
        );
    }
}
