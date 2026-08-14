//! Shared conveniences for native plugins that are not themselves engine
//! surface — the "every plugin ends up writing this once" layer.
//!
//! Two conveniences live here because they are the same shape (small,
//! self-contained, low-risk work a plugin author would otherwise reimplement)
//! rather than because they are related features:
//!
//! - [`paths`]/[`config`] — a per-plugin data directory and a minimal typed
//!   config-loading helper, mirroring `JavaPlugin.getDataFolder()`/
//!   `getConfig()`.
//! - [`persistent_data`]'s non-persistent half — an in-memory, namespaced
//!   key-value store attachable to an entity or a chunk, mirroring Bukkit's
//!   `PersistentDataContainer`/`Metadatable.setMetadata`. The *persistent*
//!   half (surviving a restart) is out of scope until world persistence
//!   exists at all — see that module's doc for the hazard to avoid when
//!   someone builds it.
//!
//! See [`docs/plugin-api.md`](../../../../docs/plugin-api.md) for the plugin
//! ABI this crate sits on top of, and `crates/plugins/README.md` for what
//! belongs in `crates/plugins/` at all — this crate is exactly that: useful to
//! a plugin, not part of the engine, and it would leave a working client if
//! deleted.

pub mod config;
pub mod paths;
pub mod persistent_data;

pub use persistent_data::{ChunkDataStore, EntityDataStore, PersistentDataPlugin, namespaced_key};
