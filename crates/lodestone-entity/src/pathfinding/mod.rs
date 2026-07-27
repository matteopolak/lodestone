//! Vanilla-parity land pathfinding.
//!
//! This module reproduces the behaviour of Minecraft's ground pathfinder:
//!
//! * [`PathFinder`] — the A* search over a [`heap::BinaryHeap`] open set, with
//!   vanilla's `1.5` heuristic fudge and `g + distance + malus` edge cost.
//! * [`node::PathType`] — the block classification and its danger malus table.
//! * The `WalkNodeEvaluator` logic (inside [`search`]) — neighbour generation
//!   that respects mob width/height, step-up, drop-down and water, so path
//!   validity is *per-mob*.
//! * [`Path`] and [`PathNavigator`] — a path followed over time that can stall
//!   and fail.
//!
//! The world is seen only through the [`PathWorld`] trait, so nothing here
//! depends on a version crate or on `lodestone-world`; a real adapter or a test
//! double supplies block classification and collision.

pub mod heap;
pub mod navigation;
pub mod node;
pub mod search;
pub mod world;

pub use navigation::PathNavigator;
pub use node::{Node, PathType};
pub use search::{Path, PathFinder, PathNode, PathParams, PathStart};
pub use world::{Aabb, MobShape, PathWorld};
