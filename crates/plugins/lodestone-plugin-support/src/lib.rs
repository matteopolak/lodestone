//! Shared conveniences for native plugins that are not themselves engine
//! surface — the "every plugin ends up writing this once" layer.
//!
//! Two conveniences live here because they are the same shape (small,
//! self-contained, low-risk work a plugin author would otherwise reimplement)
//! rather than because they are related features:
//!
//! - [`paths`]/[`config`] — a per-plugin data directory and a minimal typed
//!   config-loading helper, matching the familiar per-plugin-data-directory
//!   and config-loading conventions.
//! - [`persistent_data`]'s in-memory, namespaced key-value stores attachable
//!   to an entity or a chunk, matching the familiar per-object metadata
//!   convention.
//! - [`durable_data`]'s storage-neutral, versioned record store for plugin,
//!   world, player and generation-qualified entity scopes. It supplies the
//!   bounded snapshot/restore lifecycle; a world backend owns the actual
//!   transaction and file format.
//! - [`reentrancy`] (native only) — a reusable test harness for the
//!   `EcsHandle` reentrancy-deadlock class of bug, so a plugin author can
//!   check their own plugin before shipping it instead of discovering the
//!   hazard the way it shipped in production.
//!
//! See [`docs/plugin-api.md`](../../../../docs/plugin-api.md) for the plugin
//! ABI this crate sits on top of, and `crates/plugins/README.md` for what
//! belongs in `crates/plugins/` at all — this crate is exactly that: useful to
//! a plugin, not part of the engine, and it would leave a working client if
//! deleted.

pub mod config;
pub mod durable_data;
pub mod paths;
pub mod persistent_data;
// Not built for wasm32: a browser has no real threads and `std::thread::spawn`
// traps there, and this is a development-time tool a plugin author runs on
// their own machine, never code that ships inside a running client. See the
// module's own doc for the full reasoning.
#[cfg(not(target_arch = "wasm32"))]
pub mod reentrancy;

pub use durable_data::{
    plugin_data_snapshot_path, DataScope, PluginDataEntry, PluginDataError, PluginDataKey,
    PluginDataKeyError, PluginDataRecord, PluginDataStore, PLUGIN_DATA_SNAPSHOT_FILE,
};
pub use persistent_data::{ChunkDataStore, EntityDataStore, PersistentDataPlugin, namespaced_key};
