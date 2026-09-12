//! Session state, event folds, and HUD overlays.
//!
//! The implementation is split by responsibility: [`components`] contains the
//! public session components, [`ingest`] owns network-event folds and plugin
//! registration, and [`hud`] owns driver-side overlays and ticking. This file
//! remains the stable `crate::session` API through its re-exports.

mod components;
mod hud;
mod ingest;

pub use components::*;
pub use hud::*;
pub use ingest::*;

#[cfg(test)]
mod tests;
