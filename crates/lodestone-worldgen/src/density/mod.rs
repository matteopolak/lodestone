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
//! marker wrapper ever changes *what* is computed, only how many times. One of
//! the five marker kinds (`cache_2d`) does real caching here — see
//! `## Caching` below; the raw, uninstantiated `DensityFunctions.Marker.compute`
//! (`DensityFunctions.java:793-797`) is fully transparent for all five, and
//! that remains true here for the other four (`interpolated`, `flat_cache`,
//! `cache_once`, `cache_all_in_cell`) and, deliberately, for `blend_density`.
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
//!   subtree whose value is a pure function of `(x, z)` — vanilla's own
//!   `Cache2D.compute` keys on exactly `(blockX, blockZ)` and ignores `y`
//!   outright. [`Density::Cache2D`] gets a real [`Cache2DSlot`] here: a
//!   single-slot last-`(x,z)`-value cache, exact for *any* caller (see
//!   [`Cache2DSlot`]'s own doc for why a single slot, not vanilla's
//!   whole-chunk prefilled array, is the bit-exact-for-any-caller choice).
//!   Measured win: `preliminarySurfaceLevel`'s own `cache2d(offset)` /
//!   `cache2d(factor)` wrapping (`NoiseRouterData.java:489-490`) sits directly
//!   above `find_top_surface`'s per-`y` scan loop, so one corner's scan (up to
//!   ~56 candidate `y` values, `NoiseSettings.OVERWORLD_NOISE_SETTINGS`'s
//!   `[-64, 320]` range in 8-block steps) now evaluates that `(x, z)`-only
//!   subtree once instead of once per candidate `y`. A criterion
//!   `--baseline`/`--baseline` paired comparison (`docs/benchmark-harness.md`)
//!   measured **−4.4% (95% CI −6.0%..−2.7%, p < 0.05)** on `column()`'s
//!   median from this alone — real, but modest, because
//!   `preliminary_surface_level` is a minority of total column cost even
//!   within the surface stage (§ `docs/worldgen-surface-perf.md`'s corner-cell
//!   hoist already eliminated the *outer*, 256×-per-chunk redundancy; this
//!   catches the *inner*, per-`y`-step redundancy that hoist could not touch).
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

use std::sync::Mutex;

use serde_json::Value;

use crate::math::{clamp, clamped_map};
use crate::noise::{BlendedNoise, NormalNoise};
use crate::rng::{PositionalRandomFactory, RandomSource, XoroshiroRandomSource};

mod chunk;
mod spline;
pub use chunk::NoiseChunkSampler;
pub use spline::{Spline, SplinePoint};

/// A single-slot last-value `(x, z) -> f64` cache backing [`Density::Cache2D`]
/// — see the `## Caching` section on [`Density`] for which vanilla node kinds
/// this is (and, per a measured regression, is *not*) worth applying to.
///
/// `Mutex`, not `Cell`: [`Density`] must stay `Sync` — real `ChunkSource`
/// implementations (`lodestone-server`) share one generator, and therefore one
/// `Density` tree, across threads behind `&self`. A poisoned lock (only
/// possible if a panic previously unwound out of `inner.compute` while this
/// slot's lock was held) is recovered from rather than propagated: a cache is
/// not a correctness-critical invariant, so losing a poisoned slot's stale
/// entry and carrying on is preferable to panicking the whole evaluation.
///
/// `Clone` deliberately does **not** clone the cached entry — a cloned tree
/// (e.g. `OverworldGenerator::shape_stage`'s per-chunk
/// `self.final_density.clone()`) starts cold, exactly as a fresh `Builder`
/// output would. Cloning lock state across a `Mutex` isn't meaningful anyway.
/// Opaque: the only public operations are [`Default`], [`Clone`] and
/// [`Debug`] (all needed since [`Density`] itself derives them); the field
/// and [`Self::get_or_compute`] stay private to this module, so no external
/// caller can observe or depend on the cache's contents or keying.
#[derive(Debug, Default)]
pub struct Cache2DSlot(Mutex<Option<(i32, i32, f64)>>);

impl Clone for Cache2DSlot {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl Cache2DSlot {
    /// Returns the cached value for `(x, z)` if the slot's last entry was for
    /// this exact position, else computes it via `f`, stores it, and returns
    /// it. `y` is not part of the key: the node kind this backs (`cache_2d`)
    /// is, by construction, asked to cache a subtree whose value cannot
    /// depend on `y` (see `## Caching` on [`Density`]).
    fn get_or_compute(&self, x: i32, z: i32, f: impl FnOnce() -> f64) -> f64 {
        let mut slot = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((cached_x, cached_z, value)) = *slot
            && cached_x == x
            && cached_z == z
        {
            return value;
        }
        let value = f();
        *slot = Some((x, z, value));
        value
    }
}

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
    /// `cache_2d` — for point evaluation, caches the wrapped fn's value by
    /// exact `(x, z)` (module `## Caching`). For the block field this stays
    /// transparent, matching [`chunk::NoiseChunkSampler`]'s existing,
    /// JVM-cross-checked handling.
    Cache2D {
        /// Wrapped function.
        inner: Box<Density>,
        /// Last-`(x, z)`-value cache for point evaluation.
        cache: Cache2DSlot,
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
}

impl Density {
    /// Evaluates the node at `ctx`.
    #[must_use]
    pub fn compute(&self, ctx: Context) -> f64 {
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
            | Density::FlatCache { inner, .. } => inner.compute(ctx),
            Density::Cache2D { inner, cache } => {
                cache.get_or_compute(ctx.x, ctx.z, || inner.compute(ctx))
            }
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
/// exactly as vanilla's `RandomState` does for the overworld (xoroshiro).
#[allow(missing_debug_implementations)]
pub struct Builder<'a> {
    master: crate::rng::XoroshiroPositionalFactory,
    resolver: &'a dyn Resolver,
    slots: std::cell::Cell<usize>,
}

impl<'a> Builder<'a> {
    /// Creates a builder for `seed` using `resolver` for references.
    pub fn new(seed: i64, resolver: &'a dyn Resolver) -> Self {
        let master = XoroshiroRandomSource::new(seed).fork_positional();
        Self {
            master,
            resolver,
            slots: std::cell::Cell::new(0),
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
    pub fn positional_factory(&self) -> crate::rng::XoroshiroPositionalFactory {
        self.master
    }

    fn instantiate_blended(&self, node: &Value) -> BlendedNoise {
        let mut src = self.master.from_hash_of("minecraft:terrain");
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
                cache: Cache2DSlot::default(),
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
