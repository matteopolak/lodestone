//! Autonomous navigation: the version-free search core.
//!
//! `(snapshot, start, goal, policy, budget) → plan`, plus "given a plan edge and a
//! `PlayerState`, what keys do I press this tick". Everything else — scheduling,
//! arbitration, chat, the overlay — is [`lodestone-autopilot`]'s problem. That
//! boundary is what makes the search testable and the plugin thin.
//!
//! Designed against [`docs/baritone-port.md`](../../../docs/baritone-port.md);
//! implemented and documented in
//! [`docs/autonomous-navigation.md`](../../../docs/autonomous-navigation.md).
//!
//! # Provenance
//!
//! **Clean-room.** This crate carries no structure, no type names and no constants
//! from any existing navigation implementation. Every movement number comes from
//! `lodestone_physics::PhysicsProfile` — itself pinned bit-for-bit against a JVM
//! oracle and an independent Python re-implementation across 29 golden traces — or
//! is *derived by running that integrator* ([`cost`]). Block data comes from this
//! repo's own censuses, dumped by booting the real 26.2 server. See
//! `docs/baritone-port.md` §1.
//!
//! # The one idea to understand first
//!
//! Costs are **simulated, not tabulated.** [`cost::TemplateTable`] obtains a
//! movement's duration by running `lodestone_physics::tick` with
//! [`drive::WalkDrive`] — the *executor's own* input script — over a synthetic
//! stencil world, memoised by equivalence class. So the number the search believes
//! is the number the executor produced under the same inputs against the same
//! physics. A search that believes an edge takes 6 ticks while the executor needs
//! 14 is not a bug that can occur here; it is excluded by construction.
//!
//! # Layout
//!
//! | module | what |
//! |---|---|
//! | [`ticks`] | the cost unit: fixed-point ticks, so the priority queue is reproducible |
//! | [`facts`] | per-block-state facts, resolved once per session into a flat table |
//! | [`view`] | [`NavView`] (where `None` is *not* air) and [`SnapshotView`], which is also a `CollisionView` |
//! | [`graph`] | nodes carrying arrival state, and the legality predicates |
//! | [`drive`] | the input script, shared by the cost model and the executor |
//! | [`cost`] | simulated costs, memoised by equivalence class |
//! | [`goal`] | admissible goals |
//! | [`plan`] | a validated plan and its witness set |
//! | [`search`] | the resumable, budgeted, weighted A\* |
//! | [`policy`] | the knobs, captured by value into each search |
//! | [`witness`] | diffing a plan's witness set against the live world |
//!
//! # Milestone
//!
//! This is **M1**: `Walk` only (including sub-`step_height` ups and downs, so
//! bottom slabs, soul sand and farmland work), `Arrival::{Still, Walking}`,
//! `AtBlock` and `AtColumn`, one search dispatch, no segmentation. `WalkDiagonal`,
//! `StepUp`, `Descend`, `Drop`, `Climb`, `Gap`, `Break`, `Place` and `Swim` are
//! M2–M7 and each is four small edits in four named places: a [`graph::MoveKind`]
//! variant, a legality rule, a [`drive`] script, and a [`cost::TemplateKey`].
//!
//! [`lodestone-autopilot`]: https://docs.rs/lodestone-autopilot

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cost;
pub mod drive;
pub mod facts;
pub mod goal;
pub mod graph;
pub mod plan;
pub mod policy;
pub mod search;
pub mod ticks;
pub mod view;
pub mod witness;

pub use cost::{EntryRel, SpeedClass, SurfaceClass, Template, TemplateKey, TemplateTable};
pub use drive::{DriveTick, WalkDrive, axes_for_world_dir, yaw_towards};
pub use facts::{AdapterCensus, BlockCensus, BlockFacts, FactsTable, MUST_NOT_ENTER};
pub use goal::{AtBlock, AtColumn, Goal, Rates};
pub use graph::{
    Arrival, BODY_HEIGHT, ClimbDir, Dir4, MoveKind, NavNode, STEP_HEIGHT, Step, climb_step,
    seed_node, stand_surface, standable, successors,
};
pub use plan::{Edge, Plan, PlanError};
pub use policy::NavPolicy;
pub use search::{Budget, Outcome, Progress, Search, SearchStats};
pub use ticks::Ticks;
pub use view::{GridView, NavView, SnapshotView};
pub use witness::{first_change, point_state, sample as sample_witnesses};
