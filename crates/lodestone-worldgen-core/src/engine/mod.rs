//! The flattened density engine: an index-addressed graph, per-chunk reusable
//! scratch, and the block-field evaluator over both.
//!
//! ## What it is
//!
//! Unit 4 of the worldgen rewrite. A [`Program`] is a compiled
//! [`crate::density::Density`] tree — every node a fixed-width record in one
//! `Vec`, every child a `u32` index — shared by `Arc` and evaluated against a
//! pooled [`Scratch`] that holds all the mutable memoisation. It replaces a
//! recursive walk over a `Box`-linked enum whose every node was as wide as its
//! widest variant, and it replaces the per-chunk deep clone that walk required.
//!
//! ## How it works
//!
//! Three pieces, one per file:
//!
//! | file | holds | mutability |
//! |---|---|---|
//! | `graph.rs` | [`Program`], the `Op` table and the side tables | immutable, `Sync`, `Arc`-shared |
//! | `scratch.rs` | [`Scratch`] — the corner and cell caches | per-chunk, per-thread, pooled |
//! | `field.rs` | the `NoiseChunk`-semantics evaluator | borrows both |
//!
//! The split is the design: because *no* cache lives in the graph, one graph can
//! back concurrent chunk generation on any number of threads with no lock and no
//! copy, and a chunk's caches can be recycled without touching the graph.
//! `crate::density::NoiseChunkSampler` is the façade that pairs them and is what
//! callers actually hold.
//!
//! Two cache layers sit in the scratch and neither subsumes the other — see
//! `scratch.rs`'s module doc for the table. Briefly: the **cell** layer takes
//! corner *lookups* per chunk from 786,432 to 6,144, and the **slot** layer
//! keeps corner *evaluations* at their true distinct count of 1,225.
//!
//! ## How to change it
//!
//! * **The walk must stay a recursive descent.** `Mul` does not evaluate its
//!   second operand when the first is exactly `0.0`, and a skipped subtree can
//!   contain a cache-slot write, so a bottom-up sweep over the `Op` table would
//!   change what *later* queries return, not just the cost. Three other kinds
//!   branch too.
//! * **Adding a `Density` variant means three edits, and only one of them is a
//!   compile error.** The `match` in `graph.rs`'s `compile_node` is exhaustive so
//!   it will fail to build; `field.rs`'s `eval` match on `OpKind` will also fail.
//!   But `OpKind`'s discriminant must additionally equal the new variant's
//!   `Density::kind_index()`, and *that* is only caught by
//!   `graph::tests::op_kind_discriminants_match_density_kind_index`. Insert at
//!   the end of both tables, never in the middle.
//! * **Do not flatten beneath `spline` / `old_blended_noise` /
//!   `find_top_surface`.** They are leaves to this evaluator by vanilla's own
//!   semantics: it calls the *point* interpreter, so everything under them is
//!   evaluated without quart snapping or interpolation. They hold an untouched
//!   `Density` subtree for exactly that reason.
//! * **No `mul_add`, no FMA, no reassociation.** The field walk uses only
//!   IEEE-exact operations and the `Mth.lerp*` family; unlike the noise-init
//!   constants there is no 1-ulp question in it at all, and keeping it that way
//!   is a correctness property rather than a style preference.
//! * A reused [`Scratch`] keeps its buffers, so `reconfigure` clearing every
//!   presence flag is the only thing standing between the pool and a stale value
//!   from the previous chunk. That failure produces plausible terrain.
//!
//! ## Configuration
//!
//! None. No feature flag or env var selects any of this. The `gen-counters`
//! feature turns the `corner_lookups` / `density_evals` / `slot_hit` /
//! `slot_miss` hooks from inert to live.
//!
//! ## Dependencies
//!
//! `crate::density` (for the `Density` tree it compiles from and the point
//! interpreter it calls at leaves), `crate::noise`, `crate::math` and
//! `crate::counters`. Nothing outside this crate. Evidence:
//! `crates/lodestone-worldgen/tests/{chunk_parity,interpolation_order,engine_semantics}.rs`
//! and `docs/worldgen-density-engine.md`.

mod field;
mod graph;
pub mod redundancy_probe;
mod scratch;

pub(crate) use field::Field;
pub use field::Geom;
pub use graph::Program;
pub use scratch::{Bounds, Scratch};
