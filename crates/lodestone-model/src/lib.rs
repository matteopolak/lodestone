//! Canonical, version-free Minecraft Java Edition game model.
//!
//! This crate is deliberately independent of protocol crates. It models the
//! newest semantic shape of game concepts using stable namespaced identifiers
//! rather than version-specific numeric registries or packet encodings.
//!
//! # Boundary
//!
//! `lodestone-model` owns only version-free primitives, canonical
//! [`ClientEvent`] carriers, and [`ClientAction`] intents. It should not become
//! the home for every consumer's query state: aggregate structures such as a
//! scoreboard table, team index, boss-bar collection, entity view, or inventory
//! machine belong in crates that fold these events into application state.
//!
//! Keep protocol details out of this crate as well. Version adapters translate
//! packet ids, wire enum ordinals, registry numbers, and legacy omissions into
//! these stable types. When an older protocol cannot represent a modern
//! [`ClientAction`], the adapter should report
//! [`AdapterError::Unsupported`] / [`AdapterError::UnsupportedAction`] rather
//! than silently inventing lossy defaults.

pub mod action;
pub mod adapter;
pub mod common;
pub mod event;
pub mod ids;
pub mod item;
pub mod math;
pub mod path;
pub mod registry;
pub mod text;

pub use action::*;
pub use adapter::*;
pub use common::*;
pub use event::*;
pub use ids::*;
pub use item::*;
pub use math::*;
pub use path::*;
pub use registry::*;
pub use text::*;

#[cfg(test)]
mod tests;
