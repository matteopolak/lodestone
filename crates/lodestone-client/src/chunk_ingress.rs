//! Bounded observations of deferred whole-column application.

use std::sync::atomic::{AtomicU64, Ordering};

/// Cumulative whole-column ingress observations for one client session.
/// Unknown adapter paths are not included. Snapshots may overlap a packet apply.
/// Terrain equality covers column and light storage, not independent metadata.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChunkIngressStats {
    pub first_loads: u64,
    pub replacements: u64,
    pub terrain_unchanged: u64,
    pub terrain_changed: u64,
    pub comparison_ns: u64,
    pub comparison_max_ns: u64,
}

#[derive(Debug, Default)]
pub(crate) struct ChunkIngressCounters {
    first_loads: AtomicU64,
    replacements: AtomicU64,
    terrain_unchanged: AtomicU64,
    terrain_changed: AtomicU64,
    comparison_ns: AtomicU64,
    comparison_max_ns: AtomicU64,
}

impl ChunkIngressCounters {
    pub(crate) fn record(&self, replacement: Option<bool>, comparison_ns: u64) {
        match replacement {
            None => {
                self.first_loads.fetch_add(1, Ordering::Relaxed);
            }
            Some(terrain_changed) => {
                self.replacements.fetch_add(1, Ordering::Relaxed);
                let counter = if terrain_changed {
                    &self.terrain_changed
                } else {
                    &self.terrain_unchanged
                };
                counter.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.comparison_ns.fetch_add(comparison_ns, Ordering::Relaxed);
        self.comparison_max_ns.fetch_max(comparison_ns, Ordering::Relaxed);
    }

    pub(crate) fn snapshot(&self) -> ChunkIngressStats {
        ChunkIngressStats {
            first_loads: self.first_loads.load(Ordering::Relaxed),
            replacements: self.replacements.load(Ordering::Relaxed),
            terrain_unchanged: self.terrain_unchanged.load(Ordering::Relaxed),
            terrain_changed: self.terrain_changed.load(Ordering::Relaxed),
            comparison_ns: self.comparison_ns.load(Ordering::Relaxed),
            comparison_max_ns: self.comparison_max_ns.load(Ordering::Relaxed),
        }
    }
}
